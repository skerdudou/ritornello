# Serveur MPD — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** exposer l'appareil comme un serveur MPD sur le réseau local, pour
qu'un client de téléphone (M.A.L.P.) serve de télécommande.

**Architecture:** un greffon `ritornello-plugin-mpd` portant trois genres dans
un seul processus — `input` pour émettre les commandes, `display` pour tenir le
dernier état connu, `admin` pour régler l'écoute — plus un `TcpListener` sur
6600 dont chaque client est une tâche. Cinq additions hors du greffon rendent la
chose possible : volume absolu, source par nom, état de lecture publié,
énumération des présélections, trame de catalogue sur le protocole `display`.

**Tech Stack:** Rust 2021, tokio, serde, anyhow, async-trait ; Vue 3 +
`@ritornello/ui` pour la page d'admin ; vitest et vue-tsc côté IHM.

**Spec:** `docs/superpowers/specs/2026-08-24-serveur-mpd-design.md` — le plan
argumente depuis elle, les deux se lisent ensemble. Chaque tâche renvoie à sa
section, qui porte le code exact quand elle en porte.

## Global Constraints

- **Tests** : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/rendez-vous-greffons && cargo test --workspace"`.
  `cargo` n'existe **que** dans WSL sur cette machine — il n'est pas dans le
  `PATH` de Windows.
- **Lint** : `cargo clippy --workspace --all-targets -- -D warnings` doit rester
  propre **pour tout code de ce chantier**.
  `crates/ritornello-plugin-files/src/scan.rs` échoue déjà sur `main`
  (`too_many_arguments` 9/7, `manual_is_multiple_of`,
  `assertions_on_constants`) : **préexistant, ne pas le corriger**, ne pas s'en
  alarmer.
- **IHM** : `npm test --workspaces`, `npm run typecheck`, et
  `npm run build --workspaces` avant tout `cargo build` qui embarque un `dist`.
- **Langue** : commentaires et noms de tests en **français**, y compris les noms
  de fonctions de test. Messages de journal et clés i18n en **anglais**.
- **i18n** : toute clé ajoutée à un `en.toml` embarqué doit l'être au `fr.toml`
  livré. Deux tests le vérifient (un Rust, un vitest) et ils sont bloquants.
- **Pas de marge d'horloge murale dans un test.** Une propriété prouvée par une
  durée casse sous la charge des binaires de test concurrents ; c'est la classe
  de flake que le chantier précédent a éliminée. Préférer un signal non temporel
  (canal, compteur, message d'erreur distinct). Si une échéance est
  indispensable, écarter les rapports de durée d'un ordre de grandeur.
- **`tokio::time::pause()` ne convient pas** aux courses sur E/S réelle :
  l'horloge virtuelle n'avance que si le runtime se croit oisif, donc sur de
  l'E/S elle saute jusqu'à l'échéance, et tant qu'une tâche `spawn_blocking` est
  en vol elle n'avance pas du tout. Mesuré deux fois.
- **`ritornello-core` est un crate binaire sans `lib.rs`** : ses tests ne
  tournent pas tant que `main.rs` ne compile pas. Toute tâche qui touche
  `core.rs` doit laisser `main.rs` compilable dans le même commit.
- **Un test qui passe sans son correctif ne prouve rien.** Après avoir écrit un
  test censé échouer, le lancer et vérifier **qu'il échoue** ; s'il passe déjà,
  le dire dans le rapport et reformuler la propriété visée plutôt que de le
  garder tel quel.

## Structure des fichiers

Créés :

| Fichier | Responsabilité |
|---|---|
| `crates/ritornello-proto/src/display.rs` | `DisplayFrame`, `Catalogue`, `SourceCatalogue` |
| `crates/ritornello-plugin-mpd/Cargo.toml` | le crate |
| `crates/ritornello-plugin-mpd/build.rs` | bouchon de `ui/dist` pour un clone frais |
| `crates/ritornello-plugin-mpd/src/main.rs` | environnement, config, liaison TCP, câblage |
| `crates/ritornello-plugin-mpd/src/config.rs` | adresse et port : lecture, validation, écriture |
| `crates/ritornello-plugin-mpd/src/etat.rs` | état partagé, versions, sous-systèmes changés |
| `crates/ritornello-plugin-mpd/src/protocole.rs` | découpage des lignes, mise en forme, `ACK` |
| `crates/ritornello-plugin-mpd/src/commandes.rs` | une fonction par commande, **pures** |
| `crates/ritornello-plugin-mpd/src/session.rs` | la tâche par client : listes, `idle` |
| `crates/ritornello-plugin-mpd/src/admin.rs` | la page d'admin |
| `crates/ritornello-plugin-mpd/src/placeholder.rs` | recopié de `generic-input` |
| `crates/ritornello-plugin-mpd/src/locales/en.toml` | catalogue embarqué |
| `crates/ritornello-plugin-mpd/ui/` | paquet npm de la page d'admin |
| `deploy/locales/mpd/fr.toml` | pack français livré |
| `deploy/mpd.example.toml` | adresse et port par défaut |

Modifiés :

| Fichier | Quoi |
|---|---|
| `Cargo.toml` (racine) | `members` |
| `crates/ritornello-proto/src/command.rs` | `SetVolume`, `SelectSource` |
| `crates/ritornello-proto/src/metadata.rs` | `Playback`, `PlayerState.playback` |
| `crates/ritornello-proto/src/source.rs` | `Preset`, `SourceMessage.presets`, `SourceReq::ListPresets` |
| `crates/ritornello-proto/src/lib.rs` | réexports |
| `crates/ritornello-plugin-sdk/src/server.rs` | `list_presets`, `DisplayPlugin::catalogue`, enveloppe |
| `crates/ritornello-plugin-sdk/src/client.rs` | `send_catalogue`, `SourceUpdate.presets` |
| `crates/ritornello-core/src/core.rs` | `paused`, `set_volume`, `basculer_vers`, table de catalogue |
| `crates/ritornello-core/src/main.rs` | canal de catalogue, relais à deux récepteurs, demandes détachées |
| `crates/ritornello-plugin-radio/src/main.rs` | `list_presets` |
| `deploy/deploy.sh` | tableau `PLUGINS` |
| `deploy/plugins.example.toml` | un bloc `[[plugin]]` |
| `docs/plugins.md` | le greffon, le deuxième message d'affichage, `ListPresets` |

---

## Task 1 : le cœur sait s'il joue ou s'il est en pause

**Files:**
- Modify: `crates/ritornello-proto/src/metadata.rs`
- Modify: `crates/ritornello-proto/src/lib.rs`
- Modify: `crates/ritornello-core/src/core.rs`

**Interfaces:**
- Produces: `ritornello_proto::Playback` (`Stopped`/`Playing`/`Paused`),
  `PlayerState.playback`, et `Playback::est_arrete(&self) -> bool`.

Voir la spec, § « 3. `PlayerState.playback` », qui porte le code exact.

- [ ] **Step 1 : le type et le champ**

Dans `metadata.rs`, avant `PlayerState` :

```rust
/// Ce que fait le lecteur, en un mot. `Stopped` par défaut : ne rien savoir,
/// c'est ne rien jouer — la même convention que `can_eject`, où l'absence
/// d'information vaut l'absence de capacité.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Playback {
    #[default]
    Stopped,
    Playing,
    Paused,
}

impl Playback {
    /// Sert le `skip_serializing_if` du champ : la valeur par défaut ne
    /// voyage pas, donc les trames existantes restent identiques à l'octet.
    /// Une méthode et non une fermeture : `skip_serializing_if` exige un
    /// chemin de fonction.
    pub fn est_arrete(&self) -> bool {
        matches!(self, Playback::Stopped)
    }
}
```

Puis le champ dans `PlayerState`, à côté de `position_s` :

```rust
    /// Ce que fait le lecteur. Additif, à l'idiome de `InputMessage.held` et
    /// de `PluginStatus.stalled` : absent du JSON quand il vaut `Stopped`,
    /// donc aucune trame existante ne change et une trame ancienne se relit.
    ///
    /// Distinct de `position_s.is_some()` : une lecture en pause garde sa
    /// position, et un flux qui joue peut n'en avoir aucune.
    #[serde(default, skip_serializing_if = "Playback::est_arrete")]
    pub playback: Playback,
```

Réexporter `Playback` depuis `lib.rs`, dans la liste `pub use metadata::{…}`.

- [ ] **Step 2 : les tests de sérialisation, et les voir échouer**

Dans le `mod tests` de `metadata.rs` :

```rust
#[test]
fn playback_ne_voyage_pas_quand_il_est_arrete() {
    // L'idiome additif : la valeur par défaut est absente du JSON, donc les
    // trames d'avant ce champ sont inchangées à l'octet.
    let etat = PlayerState::default();
    let json = serde_json::to_string(&etat).unwrap();
    assert!(!json.contains("playback"), "playback ne devrait pas etre serialise: {json}");
}

#[test]
fn playback_voyage_en_minuscules_quand_il_dit_quelque_chose() {
    for (p, attendu) in [(Playback::Playing, "\"playback\":\"playing\""), (Playback::Paused, "\"playback\":\"paused\"")] {
        let etat = PlayerState { playback: p, ..Default::default() };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(attendu), "{attendu} absent de {json}");
        let retour: PlayerState = serde_json::from_str(&json).unwrap();
        assert_eq!(retour.playback, p);
    }
}

#[test]
fn une_trame_sans_playback_se_relit_en_arret() {
    // Compatibilité descendante : une trame ecrite avant ce champ.
    let etat: PlayerState = serde_json::from_str(r#"{"source":"radio","volume":40,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#).unwrap();
    assert_eq!(etat.playback, Playback::Stopped);
}
```

Lancer : `cargo test -p ritornello-proto playback`.
Attendu : **échec de compilation** (`Playback` inconnu) avant le Step 1, puis
succès. Si les tests sont écrits après le Step 1, vérifier au moins qu'ils
échouent en retirant temporairement le `skip_serializing_if`.

- [ ] **Step 3 : le cœur suit la pause**

Dans `core.rs`, un champ à côté de `lecture` (`core.rs:98`) :

```rust
    /// La lecture en cours est **suspendue**. N'a de sens que quand `lecture`
    /// est vrai ; `etat_lecteur` ne le consulte pas autrement.
    ///
    /// Remis à faux **au seul endroit** où `lecture` passe à vrai. C'est la
    /// doctrine que `etat_lecteur` défend déjà pour `position_s` : un point
    /// unique ne peut pas être oublié, là où cinq effacements le seraient au
    /// sixième chemin ajouté.
    paused: bool,
```

Initialiser `paused: false` là où `lecture: false` est initialisé
(`core.rs:240`). Le poser à `false` là où `lecture` passe à vrai (le `Play`
appliqué, voir le commentaire de `core.rs:440`).

Le bras `PlayPause` (`core.rs:821`) devient :

```rust
            Command::PlayPause => {
                if self.lecture {
                    self.paused = !self.paused;
                    self.player.toggle_pause().await?;
                } else {
                    // ... corps existant inchangé, commentaires compris ...
                }
            }
```

Et dans `etat_lecteur` (`core.rs:583`), à côté de `position_s` :

```rust
            // Même raison qu'au-dessus : calculé à la publication plutôt
            // qu'entretenu dans les cinq chemins qui posent `lecture = false`.
            playback: if !self.lecture || self.standby {
                Playback::Stopped
            } else if self.paused {
                Playback::Paused
            } else {
                Playback::Playing
            },
```

- [ ] **Step 4 : les tests du cœur**

Dans le `mod tests` de `core.rs`, en réemployant le constructeur du test le
plus proche qui appelle déjà `core.handle_command(Command::PlayPause)`
(`core.rs:2247`) :

```rust
#[tokio::test]
async fn la_pause_et_la_reprise_se_lisent_dans_letat_publie() {
    // Le champ le plus lu de la commande `status` de MPD : sans lui, aucun
    // client ne peut afficher le bon bouton.
    // ... construction du cœur et de sa source comme au test voisin ...
    core.handle_command(Command::PlayPause).await.unwrap(); // demarre la lecture
    assert_eq!(core.etat_lecteur().playback, Playback::Playing);
    core.handle_command(Command::PlayPause).await.unwrap();
    assert_eq!(core.etat_lecteur().playback, Playback::Paused);
    core.handle_command(Command::PlayPause).await.unwrap();
    assert_eq!(core.etat_lecteur().playback, Playback::Playing);
    core.handle_command(Command::Stop).await.unwrap();
    assert_eq!(core.etat_lecteur().playback, Playback::Stopped);
}

#[tokio::test]
async fn une_pause_ne_survit_pas_a_un_nouveau_play() {
    // Le seul effacement de `paused` est celui du `Play` applique : si on
    // l'oubliait, une pause d'hier rendrait une lecture neuve « en pause ».
    // ... jouer, mettre en pause, puis selectionner une autre preselection ...
    assert_eq!(core.etat_lecteur().playback, Playback::Playing);
}

#[tokio::test]
async fn la_veille_dit_larret_meme_si_la_pause_etait_posee() {
    // ... jouer, mettre en pause, puis Command::Power ...
    assert_eq!(core.etat_lecteur().playback, Playback::Stopped);
}
```

- [ ] **Step 5 : lancer, et vérifier**

```
cargo test -p ritornello-proto -p ritornello-core
cargo clippy -p ritornello-proto -p ritornello-core --all-targets -- -D warnings
```

- [ ] **Step 6 : commit**

```
git add -A
git commit -m "feat(proto,core): letat publie dit sil joue, sil est en pause ou sil est arrete"
```

---

## Task 2 : volume absolu et source par son nom

**Files:**
- Modify: `crates/ritornello-proto/src/command.rs`
- Modify: `crates/ritornello-core/src/core.rs`
- Modify: `web/app/src/views/remoteCommands.ts` (seulement si le typage TS des
  commandes énumère les variantes — le vérifier, ne rien changer sinon)

**Interfaces:**
- Produces: `Command::SetVolume(u8)`, `Command::SelectSource(String)`,
  `Core::basculer_vers(cible: String) -> Result<()>`.

Voir la spec, § « 1. `Command::SetVolume(u8)` » et § « 2.
`Command::SelectSource(String)` », qui portent le code exact des deux bras et de
l'extraction.

- [ ] **Step 1 : les deux variantes**

Dans `command.rs`, à la suite de `SeekTo` :

```rust
    /// Volume absolu, en pourcent. Sert le `setvol` de MPD ; aucune touche
    /// physique ne l'émet — même raison d'être que `SeekTo`.
    ///
    /// Empiler des `VolumeUp` ne remplacerait pas cette commande : le pas est
    /// un réglage et non une constante, et chaque pas écrit une incrustation à
    /// l'écran.
    SetVolume(u8),
    /// Source désignée par son **nom**, là où `SourceCycle` ne sait qu'avancer
    /// d'un cran. Sert le `load "radio"` de MPD.
    ///
    /// Un nom inconnu est ignoré en silence par le cœur, comme une touche non
    /// liée : c'est l'émetteur qui sait ce qu'il propose.
    SelectSource(String),
```

- [ ] **Step 2 : les tests de sérialisation, et les voir échouer**

```rust
#[test]
fn roundtrip_des_commandes_a_valeur_absolue() {
    for (cmd, attendu) in [
        (Command::SetVolume(40), r#"{"cmd":"SetVolume","arg":40}"#),
        (Command::SelectSource("radio".into()), r#"{"cmd":"SelectSource","arg":"radio"}"#),
    ] {
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, attendu);
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    }
}
```

`cargo test -p ritornello-proto roundtrip_des_commandes_a_valeur_absolue` doit
échouer à la compilation avant le Step 1.

- [ ] **Step 3 : le volume absolu dans le cœur**

Scinder `step_volume` (`core.rs:653`) en deux, exactement comme dans la spec, et
ajouter le bras :

```rust
            Command::SetVolume(v) => {
                // Pas de `volume_deadline` a rearmer : ce n'est pas une touche,
                // rien ne peut etre maintenu.
                self.set_volume(v).await?;
            }
```

- [ ] **Step 4 : l'extraction de `basculer_vers`**

Déplacer le corps de `Command::SourceCycle` (`core.rs:905-957`) dans
`async fn basculer_vers(&mut self, cible: String) -> Result<()>`, **sans
retoucher un seul de ses commentaires** — chacun décrit une leçon payée. Le
`self.active_source = next_name` devient `self.active_source = cible`, et le
calcul de la cible remonte dans les deux bras d'appel, tels que la spec les
écrit.

- [ ] **Step 5 : les tests du cœur**

```rust
#[tokio::test]
async fn le_volume_absolu_remplace_le_volume_et_le_borne() {
    // ... construire le cœur ...
    core.handle_command(Command::SetVolume(40)).await.unwrap();
    assert_eq!(core.etat_lecteur().volume, 40);
    core.handle_command(Command::SetVolume(200)).await.unwrap();
    assert_eq!(core.etat_lecteur().volume, 100, "borne haute");
    core.handle_command(Command::SetVolume(0)).await.unwrap();
    assert_eq!(core.etat_lecteur().volume, 0);
}

#[tokio::test]
async fn le_volume_absolu_ecrit_une_incrustation_comme_le_pas_relatif() {
    // Un volume change depuis le reseau doit s'annoncer a l'ecran comme celui
    // change depuis la telecommande.
    core.handle_command(Command::SetVolume(40)).await.unwrap();
    assert!(core.etat_lecteur().overlay.is_some());
}

#[tokio::test]
async fn la_source_par_son_nom_bascule_comme_le_cycle() {
    // ... deux sources cablees, `radio` active ...
    core.handle_command(Command::SelectSource("cd".into())).await.unwrap();
    assert_eq!(core.active_source(), "cd");
}

#[tokio::test]
async fn une_source_inconnue_est_ignoree_sans_rien_couper() {
    // La garde qui compte : sans elle, un nom errant viderait la source active.
    core.handle_command(Command::SelectSource("nexistepas".into())).await.unwrap();
    assert_eq!(core.active_source(), "radio");
}

#[tokio::test]
async fn selectionner_la_source_deja_active_ne_coupe_pas_ce_qui_joue() {
    // C'est exactement ce qu'un client MPD envoie en rouvrant son ecran : un
    // `load` redondant ne doit pas arreter la lecture.
    // ... jouer sur `radio`, puis SelectSource("radio") ...
    assert_eq!(core.etat_lecteur().playback, Playback::Playing);
}

#[tokio::test]
async fn le_cycle_de_source_se_comporte_exactement_comme_avant_lextraction() {
    // Filet de l'extraction : le corps a change de fonction, pas de sens.
    // ... reprendre les assertions du test existant de SourceCycle ...
}
```

- [ ] **Step 6 : les autres consommateurs de `Command`**

Chercher tout `match` exhaustif sur `Command` que l'ajout casse :

```
cargo build --workspace 2>&1 | grep -A 5 "non-exhaustive\|E0004"
```

Traiter chaque site trouvé. La télécommande de la SPA n'a **pas** à offrir ces
deux commandes (aucune touche ne les porte) ; si son typage TypeScript énumère
les variantes, les y ajouter sans créer de bouton.

- [ ] **Step 7 : lancer, et vérifier**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run typecheck
```

- [ ] **Step 8 : commit**

```
git add -A
git commit -m "feat(proto,core): volume absolu et selection dune source par son nom"
```

---

## Task 3 : le crate, la config, le port lié, l'annonce

Le greffon démarre, lie 6600, s'annonce en `input` + `display`, et ne répond
encore à rien. Vérifiable de bout en bout, et c'est la brique sur laquelle tout
le reste se pose.

**Files:**
- Create: `crates/ritornello-plugin-mpd/Cargo.toml`
- Create: `crates/ritornello-plugin-mpd/src/main.rs`
- Create: `crates/ritornello-plugin-mpd/src/config.rs`
- Create: `deploy/mpd.example.toml`
- Modify: `Cargo.toml` (racine, `members`)

**Interfaces:**
- Produces: `Config { listen: String, port: u16 }`,
  `Config::charger(&Path) -> Config`, `Config::valider(&self) -> Result<(), String>`,
  `Config::enregistrer(&self, &Path) -> Result<(), String>`.
- Consumes: `ritornello_plugin_sdk::Runtime` (`.input()`, `.display()`, `.run()`).

- [ ] **Step 1 : le Cargo.toml, calqué sur `radio`**

`radio` est le bon modèle : même forme que `console` plus `serde`, `toml` et
`ritornello-i18n`, dont ce greffon aura besoin pour sa page d'admin.

```toml
[package]
name = "ritornello-plugin-mpd"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ritornello-plugin-mpd"
path = "src/main.rs"

[dependencies]
anyhow = "1"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "sync", "io-util", "time"] }
tracing = "0.1"
tracing-subscriber = "0.3"
ritornello-proto = { path = "../ritornello-proto" }
ritornello-plugin-sdk = { path = "../ritornello-plugin-sdk" }
ritornello-i18n = { path = "../ritornello-i18n" }

[dev-dependencies]
tempfile = "3"
```

Les caractéristiques de tokio sont **énumérées** et non `full` : `net` pour le
`TcpListener`, `io-util` pour les lignes, `sync` pour le `RwLock` et le
`Notify`, `time` pour les délais. La leçon est écrite dans le `Cargo.toml` de
`files` — « une dépendance qu'on ne demande pas est une dépendance qu'un jour on
perd » — et elle vaut pour les caractéristiques.

Ajouter `"crates/ritornello-plugin-mpd"` aux `members` de la racine.

- [ ] **Step 2 : les tests de la config, et les voir échouer**

Dans `config.rs`, un `mod tests` :

```rust
#[test]
fn une_config_absente_donne_les_defauts() {
    // Un fichier manquant n'est pas une erreur : le greffon doit demarrer
    // ecoutant 0.0.0.0:6600 sans qu'on ait rien provisionne.
    let c = Config::charger(std::path::Path::new("/nexiste/pas.toml"));
    assert_eq!(c.listen, "0.0.0.0");
    assert_eq!(c.port, 6600);
}

#[test]
fn une_config_partielle_complete_par_les_defauts() {
    let c: Config = toml::from_str("port = 6601").unwrap();
    assert_eq!(c.listen, "0.0.0.0");
    assert_eq!(c.port, 6601);
}

#[test]
fn le_port_zero_est_refuse() {
    // 0 demanderait au noyau un port libre : le client ne saurait pas lequel.
    let c = Config { listen: "0.0.0.0".into(), port: 0 };
    assert!(c.valider().is_err());
}

#[test]
fn une_adresse_vide_est_refusee() {
    let c = Config { listen: String::new(), port: 6600 };
    assert!(c.valider().is_err());
}

#[test]
fn lenregistrement_est_atomique_et_relisible() {
    // Ecriture par fichier temporaire puis renommage : une coupure de courant
    // ne laisse jamais un toml tronque a la place du bon.
    let dir = tempfile::tempdir().unwrap();
    let chemin = dir.path().join("mpd.toml");
    let c = Config { listen: "127.0.0.1".into(), port: 6601 };
    c.enregistrer(&chemin).unwrap();
    assert_eq!(Config::charger(&chemin), c);
    assert!(!dir.path().join("mpd.toml.tmp").exists(), "le temporaire ne survit pas");
}

#[test]
fn un_toml_illisible_ne_fait_pas_echouer_le_demarrage() {
    // Meme politique que les stations de la radio : on retombe sur les defauts
    // en journalisant, plutot que de refuser de demarrer.
    let dir = tempfile::tempdir().unwrap();
    let chemin = dir.path().join("mpd.toml");
    std::fs::write(&chemin, "ceci n'est pas du toml =").unwrap();
    assert_eq!(Config::charger(&chemin), Config::default());
}
```

`cargo test -p ritornello-plugin-mpd` doit d'abord échouer à la compilation.

- [ ] **Step 3 : la config**

```rust
//! L'adresse et le port d'écoute du serveur MPD.
//!
//! Un fichier absent ou illisible retombe sur les défauts en journalisant :
//! c'est la politique de `Stations::load` côté radio, et elle vaut ici pour la
//! même raison — un greffon qui refuse de démarrer pour un fichier mal formé
//! disparaît de la page de statut au lieu d'y expliquer son problème.

use serde::{Deserialize, Serialize};
use std::path::Path;

fn listen_defaut() -> String {
    "0.0.0.0".to_string()
}

fn port_defaut() -> u16 {
    6600
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Adresse d'écoute. `0.0.0.0` par défaut, comme le serveur web de
    /// l'appareil : la même surface, déjà exposée.
    #[serde(default = "listen_defaut")]
    pub listen: String,
    #[serde(default = "port_defaut")]
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self { listen: listen_defaut(), port: port_defaut() }
    }
}

impl Config {
    pub fn charger(chemin: &Path) -> Self {
        let texte = match std::fs::read_to_string(chemin) {
            Ok(t) => t,
            Err(e) => {
                tracing::info!("no config at {}: {e}; using defaults", chemin.display());
                return Self::default();
            }
        };
        match toml::from_str::<Self>(&texte) {
            Ok(c) => match c.valider() {
                Ok(()) => c,
                Err(raison) => {
                    tracing::warn!("invalid config at {}: {raison}; using defaults", chemin.display());
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!("unreadable config at {}: {e}; using defaults", chemin.display());
                Self::default()
            }
        }
    }

    /// Rend une **clé** de catalogue, pas une phrase : la page d'admin la
    /// traduit. Même convention que les refus de la radio.
    pub fn valider(&self) -> Result<(), String> {
        if self.listen.trim().is_empty() {
            return Err("listen_empty".into());
        }
        if self.port == 0 {
            return Err("port_zero".into());
        }
        Ok(())
    }

    pub fn enregistrer(&self, chemin: &Path) -> Result<(), String> {
        self.valider()?;
        let texte = toml::to_string_pretty(self).map_err(|_| "save_failed".to_string())?;
        // Temporaire puis renommage : le renommage est atomique sur le même
        // système de fichiers, donc aucune coupure ne laisse un toml tronqué à
        // la place du bon.
        let tmp = chemin.with_extension("toml.tmp");
        std::fs::write(&tmp, texte).map_err(|_| "save_failed".to_string())?;
        std::fs::rename(&tmp, chemin).map_err(|_| "save_failed".to_string())?;
        Ok(())
    }
}
```

- [ ] **Step 4 : `main.rs`, avec le TCP lié avant l'annonce**

L'ordre est la propriété qui compte, et la spec l'explique : un port déjà pris
fait échouer le greffon **avant** qu'il s'annonce, donc la page de statut le
montre mort au lieu de le laisser deviner.

Pour cette tâche, le greffon accepte les connexions et **ferme aussitôt** — la
session viendra en Task 8. Le `AfficheurMpd` conserve la dernière trame reçue,
le `EntreeMpd` attend sur un canal que personne n'alimente encore.

```rust
mod config;

use anyhow::Result;
use config::Config;
use ritornello_plugin_sdk::Runtime;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

fn env_ou(cle: &str, defaut: &str) -> String {
    std::env::var(cle).unwrap_or_else(|_| defaut.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let chemin = PathBuf::from(env_ou("RITORNELLO_MPD_CONFIG", "/etc/ritornello/mpd.toml"));
    let config = Config::charger(&chemin);

    // **Lié avant l'annonce.** C'est la même doctrine que le SDK tient pour ses
    // sockets Unix — lier d'abord, annoncer ensuite — et elle donne ici un
    // comportement utile : un port 6600 déjà pris fait échouer le greffon sans
    // qu'il s'annonce, donc le cœur le rapporte mort avant annonce et la page
    // de statut le montre. Sinon un port occupé se devinerait dans les
    // journaux.
    let ecoute = TcpListener::bind((config.listen.as_str(), config.port)).await?;
    tracing::info!("mpd server listening on {}:{}", config.listen, config.port);

    let etat = Arc::new(EtatPartage::default());
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(accepter(ecoute, etat.clone(), cmd_tx));

    Runtime::from_args()?
        .input(EntreeMpd { rx: cmd_rx })?
        .display(AfficheurMpd { etat })?
        .run()
        .await
}
```

`deploy/mpd.example.toml` :

```toml
# Adresse et port du serveur MPD. Tout client MPD du reseau local peut
# commander l'appareil : pas de mot de passe, comme n'importe quelle
# telecommande de la piece.
listen = "0.0.0.0"
port = 6600
```

- [ ] **Step 5 : lancer, et vérifier**

```
cargo test -p ritornello-plugin-mpd
cargo clippy -p ritornello-plugin-mpd --all-targets -- -D warnings
```

- [ ] **Step 6 : commit**

```
git add -A
git commit -m "feat(mpd): le crate du greffon, sa config, et le port lie avant lannonce"
```

---

## Task 4 : `protocole.rs` — les lignes, les guillemets, les `ACK`

Aucune E/S, aucune horloge. Le module le plus facile à tester et celui dont tout
le reste dépend.

**Files:**
- Create: `crates/ritornello-plugin-mpd/src/protocole.rs`
- Modify: `crates/ritornello-plugin-mpd/src/main.rs` (`mod protocole;`)

**Interfaces:**
- Produces: `decouper(&str) -> Result<Vec<String>, Ack>`, `Ack`,
  `ack(Ack, usize, &str, &str) -> String`, `ligne(&str, impl Display) -> String`.

- [ ] **Step 1 : les tests, et les voir échouer**

```rust
#[test]
fn les_arguments_simples_se_decoupent_sur_les_espaces() {
    assert_eq!(decouper("status").unwrap(), vec!["status"]);
    assert_eq!(decouper("play 3").unwrap(), vec!["play", "3"]);
    // Les espaces multiples ne produisent pas d'argument vide.
    assert_eq!(decouper("play   3").unwrap(), vec!["play", "3"]);
}

#[test]
fn un_argument_entre_guillemets_garde_ses_espaces() {
    assert_eq!(
        decouper(r#"load "France Inter""#).unwrap(),
        vec!["load", "France Inter"]
    );
}

#[test]
fn les_echappements_dans_les_guillemets() {
    // `\"` est un guillemet litteral, `\\` une contre-oblique litterale.
    assert_eq!(decouper(r#"load "un \"nom\"""#).unwrap(), vec!["load", r#"un "nom""#]);
    assert_eq!(decouper(r#"load "a\\b""#).unwrap(), vec!["load", r"a\b"]);
}

#[test]
fn un_guillemet_non_ferme_est_un_argument_invalide() {
    assert_eq!(decouper(r#"load "France"#), Err(Ack::Arg));
}

#[test]
fn une_ligne_vide_ne_donne_aucun_argument() {
    assert!(decouper("").unwrap().is_empty());
    assert!(decouper("   ").unwrap().is_empty());
}

#[test]
fn un_argument_vide_entre_guillemets_est_legal() {
    // `listplaylistinfo ""` doit arriver comme un nom vide, pas disparaitre.
    assert_eq!(decouper(r#"listplaylistinfo """#).unwrap(), vec!["listplaylistinfo", ""]);
}

#[test]
fn lack_porte_son_code_son_indice_et_sa_commande() {
    assert_eq!(
        ack(Ack::NoExist, 0, "load", "no such playlist"),
        "ACK [50@0] {load} no such playlist"
    );
    // L'indice est le rang dans une liste de commandes.
    assert_eq!(
        ack(Ack::Arg, 2, "setvol", "invalid volume"),
        "ACK [2@2] {setvol} invalid volume"
    );
}
```

- [ ] **Step 2 : l'implémentation**

```rust
//! La forme du protocole MPD : découper une ligne de commande, mettre en forme
//! les réponses et les refus. Aucune E/S ici — c'est ce qui rend tout le reste
//! testable sans socket.

use std::fmt::Display;

/// Les seuls codes d'erreur que ce serveur emploie. Les valeurs sont celles de
/// `ack.h` de MPD et ne peuvent pas changer : les clients les lisent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    /// Argument absent, non numérique, ou hors bornes.
    Arg = 2,
    /// Commande inconnue **ou** volontairement non gérée. MPD ne distingue pas
    /// les deux, et c'est tant mieux : `commands` dit déjà ce qui existe.
    Unknown = 5,
    /// Liste enregistrée nommée qui n'existe pas.
    NoExist = 50,
}

/// `ACK [<code>@<indice>] {<commande>} <message>`. `indice` est le rang de la
/// commande dans une liste de commandes, 0 hors liste.
pub fn ack(code: Ack, indice: usize, commande: &str, message: &str) -> String {
    format!("ACK [{}@{indice}] {{{commande}}} {message}", code as u16)
}

/// Une ligne `clé: valeur` de réponse.
pub fn ligne(cle: &str, valeur: impl Display) -> String {
    format!("{cle}: {valeur}")
}

/// Découpe une ligne de commande. Les arguments sont séparés par des espaces ;
/// un argument entre guillemets doubles peut en contenir, et `\"` comme `\\` y
/// sont des littéraux.
///
/// Un guillemet non fermé est `Ack::Arg` et non une tolérance : accepter la
/// ligne ferait exécuter une commande dont l'argument est tronqué, ce qui est
/// pire qu'un refus lisible.
pub fn decouper(ligne: &str) -> Result<Vec<String>, Ack> {
    let mut args = Vec::new();
    let mut chars = ligne.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == ' ' || c == '\t' {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut arg = String::new();
            loop {
                match chars.next() {
                    None => return Err(Ack::Arg),
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        None => return Err(Ack::Arg),
                        Some(e) => arg.push(e),
                    },
                    Some(autre) => arg.push(autre),
                }
            }
            args.push(arg);
        } else {
            let mut arg = String::new();
            while let Some(&c) = chars.peek() {
                if c == ' ' || c == '\t' {
                    break;
                }
                arg.push(c);
                chars.next();
            }
            args.push(arg);
        }
    }
    Ok(args)
}
```

- [ ] **Step 3 : lancer, et commit**

```
cargo test -p ritornello-plugin-mpd protocole
cargo clippy -p ritornello-plugin-mpd --all-targets -- -D warnings
git add -A
git commit -m "feat(mpd): decoupage des lignes de commande et mise en forme des refus"
```

---

## Task 5 : `etat.rs` — l'état partagé et les réveils

Le point délicat du greffon, et il n'est pas dans le protocole : un client qui
envoie `idle` juste après un changement doit repartir **immédiatement**, pas
attendre le changement suivant. Un `Notify` seul perd ce réveil.

**Files:**
- Create: `crates/ritornello-plugin-mpd/src/etat.rs`
- Modify: `crates/ritornello-plugin-mpd/src/main.rs` (`mod etat;`, `AfficheurMpd`)

**Interfaces:**
- Produces: `Sujet` (`Player`/`Mixer`/`Playlist`/`StoredPlaylist`),
  `Instantane`, `EtatPartage::{lire, appliquer_etat, appliquer_catalogue,
  acter_optimiste, versions, attendre}`.

- [ ] **Step 1 : les tests, et les voir échouer**

Tous sans horloge : les réveils sont prouvés par les compteurs, jamais par une
attente réussie dans un délai.

```rust
#[tokio::test]
async fn une_trame_qui_change_le_volume_reveille_mixer_et_pas_playlist() {
    let e = EtatPartage::default();
    let avant = e.versions();
    e.appliquer_etat(PlayerState { volume: 40, ..Default::default() });
    let apres = e.versions();
    assert_ne!(avant[Sujet::Mixer as usize], apres[Sujet::Mixer as usize]);
    assert_eq!(avant[Sujet::Playlist as usize], apres[Sujet::Playlist as usize]);
}

#[tokio::test]
async fn une_trame_identique_ne_reveille_personne() {
    // Le cœur deduplique deja, mais une reconnexion renvoie l'etat courant :
    // il ne doit pas passer pour un changement.
    let e = EtatPartage::default();
    e.appliquer_etat(PlayerState { volume: 40, ..Default::default() });
    let avant = e.versions();
    e.appliquer_etat(PlayerState { volume: 40, ..Default::default() });
    assert_eq!(avant, e.versions());
}

#[tokio::test]
async fn un_changement_de_source_reveille_playlist() {
    // La file d'attente EST la liste des preselections de la source active :
    // changer de source change la file.
    let e = EtatPartage::default();
    e.appliquer_etat(PlayerState { source: "radio".into(), ..Default::default() });
    let avant = e.versions();
    e.appliquer_etat(PlayerState { source: "cd".into(), ..Default::default() });
    assert_ne!(avant[Sujet::Playlist as usize], e.versions()[Sujet::Playlist as usize]);
}

#[tokio::test]
async fn la_version_de_file_est_monotone() {
    // Jamais remise a zero : un client qui compare croirait n'avoir rien
    // manque.
    let e = EtatPartage::default();
    let mut precedente = e.lire().version_file;
    for source in ["radio", "cd", "radio"] {
        e.appliquer_etat(PlayerState { source: source.into(), ..Default::default() });
        let v = e.lire().version_file;
        assert!(v > precedente, "{v} devrait depasser {precedente}");
        precedente = v;
    }
}

#[tokio::test]
async fn un_changement_survenu_avant_lattente_ne_se_perd_pas() {
    // LE test qui compte : la session lit les versions, un changement arrive,
    // *ensuite* elle s'endort. Elle doit repartir aussitot. Avec un `Notify`
    // seul, ce reveil serait perdu et le client resterait muet jusqu'au
    // changement suivant.
    let e = EtatPartage::default();
    let vues = e.versions();
    e.appliquer_etat(PlayerState { volume: 40, ..Default::default() });
    // Pas de `timeout` ici : si l'attente bloque, le test pend et l'echec est
    // franc. Une marge d'horloge serait un flake en puissance.
    let changes = e.attendre(&[Sujet::Mixer], vues).await;
    assert_eq!(changes, vec![Sujet::Mixer]);
}

#[tokio::test]
async fn lattente_ne_rend_que_les_sujets_demandes() {
    let e = EtatPartage::default();
    let vues = e.versions();
    e.appliquer_etat(PlayerState { volume: 40, source: "cd".into(), ..Default::default() });
    let changes = e.attendre(&[Sujet::Mixer], vues).await;
    assert_eq!(changes, vec![Sujet::Mixer], "playlist a change mais n'etait pas demande");
}

#[tokio::test]
async fn letat_optimiste_devance_la_trame_puis_lui_cede() {
    // La course de `pause` : le greffon acte la bascule des qu'il l'emet, et la
    // trame suivante fait autorite.
    let e = EtatPartage::default();
    e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() });
    e.acter_optimiste(&[Command::PlayPause]);
    assert_eq!(e.lire().playback(), Playback::Paused, "acte avant la trame");
    e.appliquer_etat(PlayerState { playback: Playback::Playing, ..Default::default() });
    assert_eq!(e.lire().playback(), Playback::Playing, "la trame fait autorite");
}
```

- [ ] **Step 2 : l'implémentation**

Points de conception à respecter :

- `Sujet` est un `enum` `#[repr(usize)]` de quatre valeurs servant d'indice dans
  un `[u64; 4]`. Un tableau et non une table associative : quatre sujets connus à
  la compilation, et l'indexation ne peut pas échouer.
- `versions()` rend une copie du tableau. `attendre(&[Sujet], vues: [u64; 4])`
  compare **d'abord**, et n'attend le `Notify` que si rien n'a bougé — c'est
  cette comparaison préalable qui interdit le réveil manqué, pas la notification.
- `appliquer_etat` compare champ par champ pour décider quels sujets bougent :
  `volume`/`muted` → `Mixer` ; `source` → `Playlist` **et** `Player` ;
  `playback`/`preset`/`position_s`/`morceau` → `Player`. La comparaison se fait
  sur l'ancien état complet, donc une trame identique n'incrémente rien.
- `appliquer_etat` **écrase** `playback_optimiste` avec ce que dit la trame : la
  trame fait autorité, l'optimisme n'est qu'un pont.
- `version_file` s'incrémente exactement quand `Playlist` bouge.
- `acter_optimiste(&[Command])` ne connaît que deux commandes : `PlayPause`
  (bascule `Playing`↔`Paused`, sans effet si `Stopped`) et `SetVolume` (pose le
  volume). Tout le reste est ignoré — deviner l'effet d'un `Select` sur la
  position serait faux plus souvent que juste.
- Le verrou est un `tokio::sync::RwLock` : les sessions lisent presque
  uniquement, et composer une réponse de 51 lignes ne doit pas retarder les
  autres.

- [ ] **Step 3 : brancher l'afficheur**

`AfficheurMpd::show` appelle `etat.appliquer_etat(state)` et rend `Ok(())`. Rien
d'autre : la moitié `display` n'est qu'un robinet.

- [ ] **Step 4 : lancer, et commit**

```
cargo test -p ritornello-plugin-mpd
cargo clippy -p ritornello-plugin-mpd --all-targets -- -D warnings
git add -A
git commit -m "feat(mpd): etat partage, compteurs par sujet, et reveils qui ne se perdent pas"
```

---

## Task 6 : `commandes.rs` — ce qu'on demande au serveur

Les commandes de lecture seule, plus la réponse aux commandes non gérées. Aucune
E/S, aucune horloge : un instantané en entrée, des lignes en sortie. C'est ici
que vivent les assertions qui comptent vraiment.

**Files:**
- Create: `crates/ritornello-plugin-mpd/src/commandes.rs`
- Modify: `crates/ritornello-plugin-mpd/src/main.rs` (`mod commandes;`)

**Interfaces:**
- Produces: `Issue`, `traiter(&Instantane, usize, &[String]) -> Issue`,
  `COMMANDES: &[&str]`, `file_attente(&Instantane) -> Vec<Entree>`,
  `Entree { index: u8, nom: String }`.

- [ ] **Step 1 : le type de sortie**

```rust
/// Ce que le traitement d'une commande demande à la session de faire.
///
/// La décision est **pure** et l'application impure : ce module choisit, la
/// session écrit sur la chaussette et pousse sur le canal. C'est ce qui rend la
/// table de correspondance vérifiable au test unitaire.
#[derive(Debug, PartialEq)]
pub enum Issue {
    /// Ces lignes, puis `OK` — que la session pose, pas nous : dans une liste
    /// de commandes, un seul `OK` clôt l'ensemble.
    Repondre { lignes: Vec<String>, cmds: Vec<Command> },
    /// `ACK` déjà mis en forme. Dans une liste, elle interrompt la suite.
    Refuser(String),
    /// `idle` : attendre l'un de ces sujets.
    Attendre(Vec<Sujet>),
    /// `noidle` reçu hors attente : `OK` sec.
    Annuler,
    /// `close` : `OK` puis fermeture.
    Fermer,
}

impl Issue {
    /// `OK` sec.
    pub fn ok() -> Self {
        Issue::Repondre { lignes: Vec::new(), cmds: Vec::new() }
    }
    pub fn lignes(lignes: Vec<String>) -> Self {
        Issue::Repondre { lignes, cmds: Vec::new() }
    }
    /// `OK` sec, plus une commande à émettre vers le cœur.
    pub fn agir(cmd: Command) -> Self {
        Issue::Repondre { lignes: Vec::new(), cmds: vec![cmd] }
    }
}
```

- [ ] **Step 2 : la file d'attente, dense sur des indices creux**

C'est le piège du chantier, celui qui fait le bug classique. À écrire d'abord,
avec son test.

```rust
/// Une entrée de la file d'attente : son indice de présélection (**creux**,
/// base 1, celui que `Command::Select` attend) et son nom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entree {
    pub index: u8,
    pub nom: String,
}

/// La file d'attente MPD : les présélections de la source active.
///
/// Deux cas, et la différence n'est pas cosmétique. Quand la source sait
/// énumérer, les indices peuvent être **creux** — `Stations::preset_count` rend
/// le *maximum* des numéros et non la longueur, donc des stations 1, 5 et 99 sont
/// légales. Les positions MPD, elles, sont **denses**. À défaut de liste, on
/// synthétise `1..=preset_count`, et la suite est alors dense par construction.
pub fn file_attente(inst: &Instantane) -> Vec<Entree> {
    if let Some(src) = inst.catalogue_source(&inst.etat.source) {
        if !src.presets.is_empty() {
            return src.presets.iter().map(|p| Entree { index: p.index, nom: p.name.clone() }).collect();
        }
    }
    let n = inst.etat.preset_count.unwrap_or(0);
    (1..=n).map(|i| Entree { index: i, nom: i.to_string() }).collect()
}
```

Le test qui verrouille le piège :

```rust
#[test]
fn les_positions_sont_denses_la_ou_les_indices_sont_creux() {
    // Trois stations numerotees 1, 5 et 99 : la file a trois entrees aux
    // positions 0, 1, 2, et les `Id` restent 1, 5 et 99.
    let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "France Inter"), (99, "Nova")]);
    let file = file_attente(&inst);
    assert_eq!(file.len(), 3);
    assert_eq!(file[1].index, 5, "la position 1 porte l'indice 5");
    let lignes = traiter_ok(&inst, &["playlistinfo"]);
    assert!(lignes.contains(&"Pos: 1".to_string()));
    assert!(lignes.contains(&"Id: 5".to_string()));
}

#[test]
fn sans_liste_la_file_se_synthetise_depuis_le_compte() {
    // Le cd : trois pistes, aucun nom. La suite est dense, `Pos = Id - 1`.
    let inst = instantane_sans_presets("cd", 3);
    let file = file_attente(&inst);
    assert_eq!(file, vec![
        Entree { index: 1, nom: "1".into() },
        Entree { index: 2, nom: "2".into() },
        Entree { index: 3, nom: "3".into() },
    ]);
}

#[test]
fn playlistlength_est_la_longueur_de_la_liste_pas_le_maximum_des_indices() {
    // Le bug qu'on evite : `preset_count` de la radio vaut 99 pour trois
    // stations. Annoncer 99 ferait demander 96 entrees inexistantes.
    let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "France Inter"), (99, "Nova")]);
    let lignes = traiter_ok(&inst, &["status"]);
    assert!(lignes.contains(&"playlistlength: 3".to_string()));
}
```

- [ ] **Step 3 : `status`, et ses pièges**

Ordre et présence des champs :

```
volume, repeat, random, single, consume, playlist, playlistlength,
mixrampdb, state, [song, songid], [time], [elapsed], [duration]
```

- `volume` rend **0 quand `muted`**, quel que soit le volume mémorisé. MPD n'a
  pas de sourdine, et c'est ce que le client attend de voir.
- `repeat`/`random`/`single`/`consume` rendent `0` : les clients les lisent
  toujours, et les omettre les fait mal se comporter. Les **écrire** est refusé
  (Task 7).
- `state` vaut `play`/`pause`/`stop` selon `playback()` de l'instantané, donc
  l'état **optimiste**.
- `song`/`songid` sont **absents** si rien ne joue ou si `preset` est `None` —
  absents, pas à zéro : `songid: 0` désignerait une entrée réelle.
- `elapsed` et `duration` sont en secondes décimales (`12.000`), `time` en
  entiers `elapsed:total`. `time` est déprécié mais des clients le lisent
  encore ; il n'apparaît que si `position_s` est connu.

- [ ] **Step 4 : `currentsong` et les commandes de montage**

- `currentsong` : rien du tout (`OK` sec) si `preset` est `None`. Sinon
  `file: ritornello://<source>/<indice>`, `Title` (le titre du morceau, à défaut
  le nom de la présélection), `Artist`, `Album`, `Time`, `duration`, `Pos`, `Id`.
  Un champ absent de `Morceau` ne produit **pas** de ligne — une ligne vide vaut
  pire qu'aucune.
- `playlistinfo [POS]` : la file entière, ou la seule entrée demandée
  (`ACK 2` si hors bornes).
- `plchanges <version>` : la file entière si `version` diffère de
  `version_file`, rien sinon. `ACK 2` si l'argument n'est pas un nombre.
- `commands` : `COMMANDES`, la liste réelle. **C'est la commande qui rend le
  greffon honnête** : un client correct y lit ce qui existe et grise le reste.
- `tagtypes` : `Artist`, `Album`, `Title` — les trois seuls que `Morceau` porte.
- `outputs` : `outputid: 0`, `outputname: default`, `outputenabled: 1`.
- `stats` : `songs` = longueur de la file, `artists`/`albums` = 0, `uptime` et
  `db_update` = 0. Pas d'horloge : un `uptime` réel obligerait à mémoriser un
  instant de départ pour une valeur qu'aucun client n'utilise ici.
- `decoders`, `urlhandlers` : `OK` sec, mais **présentes** — une commande
  inconnue au montage peut faire renoncer un client.
- `ping` : `OK`. `password <mot>` : `OK` sans vérifier. `close` : `Fermer`.
- Tout le reste : `Refuser(ack(Ack::Unknown, indice, cmd, "unsupported"))`.

- [ ] **Step 5 : les tests des réponses**

Un helper `instantane(...)` construit à la main, puis pour chaque commande
l'assertion sur les lignes. Les cas qui doivent y être :

```rust
#[test]
fn status_rend_zero_en_volume_quand_le_son_est_coupe() {
    // MPD n'a pas de sourdine : les clients coupent en posant `setvol 0`, donc
    // ils s'attendent a lire 0 quand c'est coupe.
    let inst = instantane_muet(65);
    assert!(traiter_ok(&inst, &["status"]).contains(&"volume: 0".to_string()));
}

#[test]
fn status_ne_nomme_aucune_chanson_a_larret() {
    // `songid: 0` designerait une entree reelle : le champ doit etre absent.
    let lignes = traiter_ok(&instantane_arrete(), &["status"]);
    assert!(lignes.contains(&"state: stop".to_string()));
    assert!(!lignes.iter().any(|l| l.starts_with("song")), "{lignes:?}");
}

#[test]
fn status_dit_les_trois_etats() { /* play, pause, stop */ }

#[test]
fn currentsong_ne_dit_rien_quand_rien_ne_joue() {
    assert_eq!(traiter_ok(&instantane_arrete(), &["currentsong"]), Vec::<String>::new());
}

#[test]
fn currentsong_omet_les_champs_inconnus_au_lieu_de_les_vider() {
    // Une station sans titre ICY : pas de ligne `Title:` vide.
    let lignes = traiter_ok(&instantane_sans_titre(), &["currentsong"]);
    assert!(!lignes.iter().any(|l| l == "Title: " || l == "Artist: "), "{lignes:?}");
}

#[test]
fn plchanges_ne_rend_rien_quand_la_version_est_a_jour() { }

#[test]
fn les_options_sont_rapportees_a_zero_mais_pas_omises() {
    let lignes = traiter_ok(&instantane_arrete(), &["status"]);
    for cle in ["repeat: 0", "random: 0", "single: 0", "consume: 0"] {
        assert!(lignes.contains(&cle.to_string()), "{cle} absent de {lignes:?}");
    }
}

#[test]
fn commands_nannonce_que_ce_qui_existe() {
    let lignes = traiter_ok(&instantane_arrete(), &["commands"]);
    assert!(lignes.contains(&"command: status".to_string()));
    // La contrepartie, celle qui rend l'annonce honnete :
    for absente in ["add", "search", "lsinfo", "save", "kill"] {
        assert!(!lignes.contains(&format!("command: {absente}")), "{absente} annoncee a tort");
    }
}

#[test]
fn une_commande_inconnue_est_refusee_avec_son_indice_de_liste() {
    let inst = instantane_arrete();
    assert_eq!(
        traiter(&inst, 3, &["nawak".to_string()]),
        Issue::Refuser("ACK [5@3] {nawak} unsupported".to_string())
    );
}

#[test]
fn les_commandes_decriture_sont_refusees_une_par_une() {
    // Elles doivent l'etre explicitement, pas par defaut : c'est la liste que
    // la doc promet, et un futur `add` accidentellement gere se verrait ici.
    for cmd in ["add", "delete", "clear", "save", "rm", "shuffle", "update", "kill",
                "repeat", "random", "single", "consume", "enableoutput"] {
        let issue = traiter(&instantane_arrete(), 0, &[cmd.to_string()]);
        assert!(matches!(issue, Issue::Refuser(_)), "{cmd} devrait etre refusee");
    }
}
```

- [ ] **Step 6 : lancer, et commit**

```
cargo test -p ritornello-plugin-mpd
cargo clippy -p ritornello-plugin-mpd --all-targets -- -D warnings
git add -A
git commit -m "feat(mpd): les commandes de lecture, et la file dense sur des indices creux"
```

---

## Task 7 : `commandes.rs` — ce qu'on demande à l'appareil

Les commandes d'action. Toujours pures : elles rendent des `Command` que la
session poussera.

**Files:**
- Modify: `crates/ritornello-plugin-mpd/src/commandes.rs`

- [ ] **Step 1 : les tests, et les voir échouer**

```rust
#[test]
fn play_choisit_la_preselection_de_cette_position() {
    // Le decalage qui coute cher : `play 1` sur des indices 1, 5, 99 doit
    // selectionner 5 — le rang, pas l'indice moins un.
    let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "France Inter"), (99, "Nova")]);
    assert_eq!(cmds(&inst, &["play", "1"]), vec![Command::Select(5)]);
}

#[test]
fn playid_prend_lindice_tel_quel() {
    let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "France Inter")]);
    assert_eq!(cmds(&inst, &["playid", "5"]), vec![Command::Select(5)]);
}

#[test]
fn play_hors_bornes_est_refuse_et_nemet_rien() {
    let inst = instantane_avec_presets("radio", &[(1, "FIP")]);
    assert!(matches!(traiter(&inst, 0, &["play".into(), "7".into()]), Issue::Refuser(_)));
}

#[test]
fn play_sans_argument_relance_ce_qui_etait_charge() {
    // La touche Lecture, pas une selection.
    let inst = instantane_arrete();
    assert_eq!(cmds(&inst, &["play"]), vec![Command::PlayPause]);
}

#[test]
fn pause_nemet_rien_quand_letat_est_deja_celui_demande() {
    // C'est ce qui ferme la course : un `pause 1` sur une lecture deja en pause
    // ne doit pas la relancer.
    let inst = instantane_en_pause();
    assert_eq!(cmds(&inst, &["pause", "1"]), Vec::<Command>::new());
    assert_eq!(cmds(&inst, &["pause", "0"]), vec![Command::PlayPause]);
}

#[test]
fn pause_sans_argument_bascule() {
    assert_eq!(cmds(&instantane_en_lecture(), &["pause"]), vec![Command::PlayPause]);
}

#[test]
fn setvol_borne_et_refuse_hors_intervalle() {
    let inst = instantane_arrete();
    assert_eq!(cmds(&inst, &["setvol", "40"]), vec![Command::SetVolume(40)]);
    assert!(matches!(traiter(&inst, 0, &["setvol".into(), "101".into()]), Issue::Refuser(_)));
    assert!(matches!(traiter(&inst, 0, &["setvol".into(), "abc".into()]), Issue::Refuser(_)));
    assert!(matches!(traiter(&inst, 0, &["setvol".into()]), Issue::Refuser(_)));
}

#[test]
fn volume_est_relatif_et_borne_sur_le_volume_courant() {
    // Commande depreciee mais encore emise. Bornee ici, pas laissee deborder.
    let inst = instantane_au_volume(95);
    assert_eq!(cmds(&inst, &["volume", "+10"]), vec![Command::SetVolume(100)]);
    assert_eq!(cmds(&instantane_au_volume(3), &["volume", "-10"]), vec![Command::SetVolume(0)]);
}

#[test]
fn seekcur_resout_le_relatif_avant_demettre_un_absolu() {
    // `Command` ne porte qu'un positionnement absolu : la resolution est ici.
    let inst = instantane_a_la_position(30);
    assert_eq!(cmds(&inst, &["seekcur", "+10"]), vec![Command::SeekTo(40)]);
    assert_eq!(cmds(&inst, &["seekcur", "-10"]), vec![Command::SeekTo(20)]);
    assert_eq!(cmds(&inst, &["seekcur", "12.5"]), vec![Command::SeekTo(12)]);
    // Un recul plus grand que la position ne produit pas de temps negatif.
    assert_eq!(cmds(&instantane_a_la_position(3), &["seekcur", "-10"]), vec![Command::SeekTo(0)]);
}

#[test]
fn seek_et_seekid_ignorent_leur_premier_argument() {
    // `Command::SeekTo` ne sait pas changer de piste en meme temps ; MPD
    // n'envoie de toute facon `seek` que sur ce qui joue.
    let inst = instantane_a_la_position(0);
    assert_eq!(cmds(&inst, &["seek", "0", "42"]), vec![Command::SeekTo(42)]);
    assert_eq!(cmds(&inst, &["seekid", "1", "42"]), vec![Command::SeekTo(42)]);
}

#[test]
fn les_touches_simples_passent_telles_quelles() {
    let inst = instantane_en_lecture();
    assert_eq!(cmds(&inst, &["next"]), vec![Command::Next]);
    assert_eq!(cmds(&inst, &["previous"]), vec![Command::Prev]);
    assert_eq!(cmds(&inst, &["stop"]), vec![Command::Stop]);
}

#[test]
fn setvol_zero_nest_pas_traduit_en_sourdine() {
    // Ce serait deviner : `Mute` bascule, `SetVolume(0)` pose. Traduire ferait
    // qu'un client remontant le volume tomberait sur un son toujours coupe.
    assert_eq!(cmds(&instantane_au_volume(65), &["setvol", "0"]), vec![Command::SetVolume(0)]);
}
```

- [ ] **Step 2 : l'implémentation**

Une fonction par commande, appelées depuis le `match` de `traiter`. Règles :

- Tout argument numérique manquant, non numérique ou hors bornes est
  `Ack::Arg` — jamais une valeur par défaut silencieuse.
- `play <POS>` indexe `file_attente()` par **rang** ; `playid <ID>` vérifie que
  l'indice existe dans la file avant d'émettre.
- `pause` sans argument bascule ; `pause 0`/`pause 1` n'émettent que si l'état
  courant diffère de la cible. Un `pause` sur un lecteur arrêté n'émet rien.
- `seekcur` accepte `+n`, `-n` et un absolu décimal ; la résolution du relatif
  se fait depuis `position_s`, tronquée en secondes, jamais négative.
- `load <nom>` attend la Task 13 : pour l'instant `Refuser(NoExist)` — le
  catalogue n'existe pas encore. Le noter en commentaire avec le renvoi à la
  tâche, pour qu'un relecteur ne le prenne pas pour un oubli.

- [ ] **Step 3 : lancer, et commit**

```
cargo test -p ritornello-plugin-mpd
cargo clippy -p ritornello-plugin-mpd --all-targets -- -D warnings
git add -A
git commit -m "feat(mpd): les commandes daction, du relatif resolu vers labsolu"
```

---

## Task 8 : `session.rs` — le dialogue

La seule partie qui touche une chaussette. Les listes de commandes et `idle` y
vivent, parce qu'ils sont des faits sur la **connexion** et non sur une commande.

**Files:**
- Create: `crates/ritornello-plugin-mpd/src/session.rs`
- Modify: `crates/ritornello-plugin-mpd/src/main.rs` (`mod session;`, `accepter`)

**Interfaces:**
- Produces: `accepter(TcpListener, Arc<EtatPartage>, Sender<InputMessage>)`,
  `servir(TcpStream, Arc<EtatPartage>, Sender<InputMessage>) -> Result<()>`.

- [ ] **Step 1 : les tests, et les voir échouer**

Le motif est celui de `register.rs:333` — **le test lie, le serveur reçoit
l'écouteur** — transposé au TCP par `TcpListener::bind("127.0.0.1:0")` puis
`local_addr()`. Aucune boucle de reprise, aucun délai : l'écouteur existe avant
que le client ne se connecte, donc un `connect` nu suffit. C'est la même
propriété que le rendez-vous des greffons a établie côté Unix.

```rust
#[tokio::test]
async fn la_banniere_arrive_sans_quon_demande_rien() {
    let (mut lignes, _w, _e) = client().await;
    let banniere = lignes.next_line().await.unwrap().unwrap();
    assert!(banniere.starts_with("OK MPD "), "banniere inattendue: {banniere}");
}

#[tokio::test]
async fn une_commande_rend_ses_lignes_puis_ok() {
    // ... envoyer "status\n", lire jusqu'a "OK" ...
    assert_eq!(*recues.last().unwrap(), "OK");
    assert!(recues.iter().any(|l| l.starts_with("volume: ")));
}

#[tokio::test]
async fn une_liste_de_commandes_ne_rend_quun_seul_ok() {
    // command_list_begin / status / status / command_list_end
    let ok = recues.iter().filter(|l| *l == "OK").count();
    assert_eq!(ok, 1, "un seul OK clot la liste: {recues:?}");
}

#[tokio::test]
async fn command_list_ok_begin_insere_un_list_ok_par_commande() {
    assert_eq!(recues.iter().filter(|l| *l == "list_OK").count(), 2);
    assert_eq!(*recues.last().unwrap(), "OK");
}

#[tokio::test]
async fn une_erreur_dans_une_liste_interrompt_la_suite() {
    // status / nawak / status : le troisieme ne doit PAS s'executer.
    // Prouve par le compte de lignes `volume:`, pas par une attente.
    assert_eq!(recues.iter().filter(|l| l.starts_with("volume: ")).count(), 1);
    assert!(recues.last().unwrap().starts_with("ACK [5@1] {nawak}"), "{recues:?}");
}

#[tokio::test]
async fn idle_ne_repond_quau_changement() {
    // Envoyer `idle`, pousser une trame qui change le volume, lire.
    // Pas de `timeout` : si rien n'arrive le test pend, et l'echec est franc.
    let l = lignes.next_line().await.unwrap().unwrap();
    assert_eq!(l, "changed: mixer");
    assert_eq!(lignes.next_line().await.unwrap().unwrap(), "OK");
}

#[tokio::test]
async fn idle_filtre_les_sujets_demandes() {
    // `idle player` ne doit pas se reveiller sur un changement de volume seul.
    // Prouve sans horloge : pousser d'abord un changement de mixer, puis un de
    // player, et verifier que la reponse nomme `player` et lui seul.
}

#[tokio::test]
async fn noidle_rend_la_main_immediatement() {
    // `idle` puis `noidle` sur la meme connexion : un `OK` sec, sans qu'aucune
    // trame n'ait bouge.
}

#[tokio::test]
async fn deux_clients_ne_se_genent_pas() {
    // LE test de l'architecture : le client A dort en `idle`, le client B
    // envoie une commande et recoit sa reponse. Prouve qu'aucune session ne
    // retient les autres, ce que le `RwLock` et une tache par client
    // garantissent.
}

#[tokio::test]
async fn une_commande_daction_arrive_sur_le_canal_dentree() {
    // `next` doit produire exactement un `InputMessage` portant `Command::Next`.
    let msg = cmd_rx.recv().await.unwrap();
    assert_eq!(msg.cmd, Command::Next);
    assert!(!msg.held, "une commande reseau n'est jamais maintenue");
}

#[tokio::test]
async fn une_ligne_illisible_ne_ferme_pas_la_connexion() {
    // Un guillemet non ferme est un `ACK 2`, pas une rupture : le client suivant
    // ne doit pas avoir a se reconnecter.
    // ... envoyer `load "France`, lire l'ACK, puis envoyer `ping` et lire OK ...
}
```

- [ ] **Step 2 : l'implémentation**

- `accepter` boucle sur `listener.accept()` et `tokio::spawn(servir(...))` par
  connexion. Une erreur d'accept est journalisée et la boucle continue : une
  connexion refusée ne doit pas emporter le serveur.
- `servir` écrit `OK MPD 0.23.5\n`, puis lit les lignes. La version annoncée est
  une constante nommée, avec en commentaire pourquoi elle est mentie : les
  clients dérivent leurs capacités de ce numéro, et annoncer une version trop
  ancienne les prive de commandes qu'on gère.
- L'état de liste vit dans la fonction : un `Option<Vec<Vec<String>>>` accumulé
  entre `command_list_begin` et `command_list_end`, plus un drapeau `avec_ok`.
- `idle` **dans une liste de commandes** est `Ack::Unknown` — MPD l'interdit, et
  l'accepter demanderait de suspendre une liste à moitié écrite.
- Après chaque `Issue::Repondre`, la session pousse les `cmds` sur le canal
  **puis** appelle `etat.acter_optimiste(&cmds)`. Dans cet ordre : le canal peut
  refuser (plein), et acter une bascule qu'on n'a pas émise serait pire que ne
  pas l'acter.
- Un `send` qui échoue sur le canal ferme la session avec un journal : la moitié
  `input` est morte, plus rien de ce que dit ce client ne peut aboutir.

- [ ] **Step 3 : lancer, et commit**

```
cargo test -p ritornello-plugin-mpd
cargo clippy -p ritornello-plugin-mpd --all-targets -- -D warnings
git add -A
git commit -m "feat(mpd): le dialogue par connexion, les listes de commandes et idle"
```

---

## Task 9 : la page d'admin, l'i18n et le déploiement

Jalon : à la fin de cette tâche, le greffon est **essayable au téléphone**.

**Files:**
- Create: `crates/ritornello-plugin-mpd/build.rs`
- Create: `crates/ritornello-plugin-mpd/src/placeholder.rs`
- Create: `crates/ritornello-plugin-mpd/src/admin.rs`
- Create: `crates/ritornello-plugin-mpd/src/locales/en.toml`
- Create: `crates/ritornello-plugin-mpd/ui/{package.json,vite.config.ts,tsconfig.json,src/index.ts,src/MpdAdmin.vue,src/i18nKeysUsed.test.ts,src/contrat.test.ts}`
- Create: `deploy/locales/mpd/fr.toml`
- Modify: `crates/ritornello-plugin-mpd/src/main.rs` (`.admin(...)`)
- Modify: `deploy/deploy.sh`, `deploy/plugins.example.toml`

- [ ] **Step 1 : recopier ce qui se recopie**

`build.rs` et `src/placeholder.rs` sont pris **verbatim** de
`crates/ritornello-plugin-generic-input/`, en changeant le seul nom qui y figure.
Ils existent parce que `ui/dist/` est ignoré par git : sans eux, `include_str!`
casse un clone frais.

Piège qui coûte une compilation : `main.rs` doit déclarer
`#[cfg(test)] mod placeholder;` — inconditionnellement, `dead_code` le refuse
sous `-D warnings`.

`ui/vite.config.ts`, `ui/package.json` et `ui/tsconfig.json` sont également
recopiés de `generic-input`. Les deux points non négociables : `vue` et
`@ritornello/ui` **externes** (carte d'import du shell, instance unique de Vue)
et sortie **plate** `ui.js` + `ui.css` — le cœur ne sert qu'un segment de
chemin, un `assets/chunk.js` serait un 404.
`scripts/verifier-dist-plugin.mjs` vérifie tout cela ; il est déjà appelé par le
`build` du modèle.

- [ ] **Step 2 : le catalogue, dans les deux langues**

`src/locales/en.toml` — les clés dont la page a besoin, plus les deux clés de
refus que `Config::valider` et `enregistrer` renvoient déjà :

```toml
title = "MPD server"
listen_label = "Listen address"
port_label = "Port"
restart_notice = "A change takes effect when the plugin restarts."
btn_save = "Save"
saved = "Saved"
listen_empty = "The listen address cannot be empty."
port_zero = "The port must be between 1 and 65535."
save_failed = "Could not save the settings."
bad_request = "Unexpected request: {detail}"
```

`deploy/locales/mpd/fr.toml` : **les mêmes clés**, traduites. Le test de parité
échoue à la moindre divergence, dans un sens comme dans l'autre.

- [ ] **Step 3 : les deux tests de parité**

Le test Rust est celui qui existe à l'identique dans cinq crates
(`generic-input/src/admin.rs:208`) — le recopier en changeant le composant et la
constante :

```rust
fn pack_fr() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/locales/mpd/fr.toml");
    std::fs::read_to_string(p).expect("pack fr livre")
}

#[test]
fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
    let en = ritornello_i18n::try_parse(crate::MPD_EN).unwrap();
    let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
    let mut cles_en: Vec<&String> = en.keys().collect();
    let mut cles_fr: Vec<&String> = fr.keys().collect();
    cles_en.sort();
    cles_fr.sort();
    assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
}
```

`ui/src/i18nKeysUsed.test.ts` et `ui/src/contrat.test.ts` sont recopiés de
`generic-input` et `files`, chemins ajustés.

- [ ] **Step 4 : la page**

Rust (`admin.rs`) : `asset` avec les deux `include_str!`, `catalog` qui rend les
entrées, `get_data` qui rend `{ "listen": …, "port": … }`, `set_data` qui
désérialise, valide et enregistre atomiquement — en renvoyant des **clés** de
catalogue, jamais un détail d'E/S.

Vue (`ui/src/MpdAdmin.vue`) : deux champs et un bouton, avec les composants du
kit (`Input`, `Label`, `Button`, `Card*`, `toast`). Les props sont
`{ catalog: Catalog; base: string }`, `base` **sans valeur par défaut** — la
raison est écrite en long dans `generic-input/ui/src/InputAdmin.vue:14-29`, et
elle vaut ici aussi. L'avis de redémarrage (`restart_notice`) est visible en
permanence, pas seulement après un enregistrement : le port ne change pas à
chaud, et le lire avant d'agir vaut mieux que de le découvrir après.

Câbler `.admin(MpdAdmin::new(...)?)` dans `main.rs`, entre `.display()` et
`.run()`.

- [ ] **Step 5 : le déploiement, avec sa garde**

`deploy/deploy.sh:14` : ajouter `mpd` au tableau `PLUGINS`, **et** le bloc
`[[plugin]]` correspondant dans `deploy/plugins.example.toml`. Le script
**refuse de déployer** si les deux ne nomment pas le même ensemble : la garde est
explicite (`deploy.sh:22-30`), donc les deux vont ensemble ou aucun.

```toml
# Serveur MPD : expose l'appareil sur le reseau local (port 6600 par defaut)
# pour qu'un client MPD de telephone serve de telecommande. Reglages dans
# /etc/ritornello/mpd.toml, page d'admin sur /plugins/mpd/.
[[plugin]]
name = "mpd"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-mpd"
```

`deploy/locales/mpd/fr.toml` ne demande **aucune** modification de script :
l'arbre `deploy/locales` est copié en entier.

- [ ] **Step 6 : lancer la chaîne complète**

```
npm ci
npm run build --workspaces
npm run typecheck
npm test --workspaces
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bash -n deploy/deploy.sh
```

- [ ] **Step 7 : commit**

```
git add -A
git commit -m "feat(mpd): page dadmin de lecoute, catalogues en/fr et declaration de deploiement"
```

---

## Task 10 : les présélections nommées et la trame de catalogue (proto + SDK)

Les additions 4 et 5, côté protocole et SDK. Voir la spec, § « 4.
`SourceReq::ListPresets` » et § « 5. La trame de catalogue », qui portent le code
exact — y compris le tableau « Où vivent les types neufs », à suivre à la lettre
pour que `Preset` n'ait pas deux définitions jumelles.

**Files:**
- Create: `crates/ritornello-proto/src/display.rs`
- Modify: `crates/ritornello-proto/src/source.rs`, `lib.rs`
- Modify: `crates/ritornello-plugin-sdk/src/server.rs`, `client.rs`

**Interfaces:**
- Produces: `Preset` (dans `source.rs`), `SourceMessage.presets`,
  `SourceReq::ListPresets`, `DisplayFrame`, `Catalogue`, `SourceCatalogue`,
  `SourcePlugin::list_presets` (défaut vide), `SourceOutcome::presets`,
  `Notification::presets`, `SourceUpdate.presets`,
  `DisplayClient::send_catalogue`, `DisplayPlugin::catalogue` (défaut ignorant).

- [ ] **Step 1 : les types, et les tests de sérialisation**

```rust
#[test]
fn lenveloppe_dune_trame_detat_porte_le_json_qui_voyageait_avant() {
    // L'etiquetage adjacent garantit que le `data` est exactement l'ancienne
    // charge utile : c'est ce qui rend la migration verifiable.
    let etat = PlayerState { source: "radio".into(), volume: 40, ..Default::default() };
    let nu = serde_json::to_value(&etat).unwrap();
    let enveloppe = serde_json::to_value(DisplayFrame::State(etat.clone())).unwrap();
    assert_eq!(enveloppe["frame"], "state");
    assert_eq!(enveloppe["data"], nu);
}

#[test]
fn les_deux_formes_de_trame_font_le_tour() {
    for trame in [
        DisplayFrame::State(PlayerState::default()),
        DisplayFrame::Catalogue(Catalogue {
            sources: vec![SourceCatalogue {
                name: "radio".into(),
                presets: vec![Preset { index: 1, name: "FIP".into() }],
            }],
        }),
    ] {
        let json = serde_json::to_string(&trame).unwrap();
        assert_eq!(serde_json::from_str::<DisplayFrame>(&json).unwrap(), trame);
    }
}

#[test]
fn une_source_sans_preselections_nommees_ne_serialise_aucune_liste() {
    let c = SourceCatalogue { name: "cd".into(), presets: Vec::new() };
    assert!(!serde_json::to_string(&c).unwrap().contains("presets"));
}

#[test]
fn list_presets_fait_le_tour_comme_requete() {
    let r = SourceReq::ListPresets;
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, r#"{"req":"ListPresets"}"#);
    assert_eq!(serde_json::from_str::<SourceReq>(&json).unwrap(), r);
}

#[test]
fn les_preselections_voyagent_a_cote_de_laction_pas_dedans() {
    // La propriete qui evite d'elargir quatre types : la reponse porte bien un
    // `action` (donc la correlation se denoue) ET la liste a cote.
    let msg = SourceMessage {
        id: Some(7),
        action: Some(SourceAction::Noop),
        presets: Some(vec![Preset { index: 1, name: "FIP".into() }]),
        ..message_vide()
    };
    let json = serde_json::to_string(&msg).unwrap();
    let retour: SourceMessage = serde_json::from_str(&json).unwrap();
    assert!(retour.action.is_some(), "sans action, le oneshot attendrait 5 s pour rien");
    assert_eq!(retour.presets.unwrap().len(), 1);
}
```

- [ ] **Step 2 : le SDK, côté source**

- `SourceOutcome` gagne `presets: Option<Vec<Preset>>` et son constructeur
  `presets(...)`, sur le modèle exact de `preset_count` (`server.rs:66`).
- `Notification` gagne le même champ et le même constructeur (`server.rs:131`).
- Le trait gagne `list_presets` **à corps par défaut**, tel que la spec l'écrit.
- Le bras de `serve_source` suit le précédent de `SetLocale`, seul autre cas
  d'une méthode qui ne rend pas de `SourceOutcome`.
- Les deux constructions de `SourceMessage` (`server.rs:291` réponse et
  `server.rs:312` spontanée) estampillent `presets`.
- `client.rs` : `SourceUpdate` gagne `presets`, le prédicat de trame
  intéressante (`client.rs:75-80`) gagne `|| msg.presets.is_some()`, et la copie
  suit (`client.rs:86`).

- [ ] **Step 3 : le SDK, côté affichage**

- `DisplayClient::send_catalogue(&Catalogue)`, jumeau de `send`, écrivant une
  `DisplayFrame::Catalogue`. `send` écrit désormais une `DisplayFrame::State`.
- `DisplayPlugin::catalogue` à corps par défaut.
- `serve_display` déserialise une `DisplayFrame` et aiguille. La politique de
  ligne illisible **ne change pas** : `warn` puis `continue`, la connexion
  survit.

- [ ] **Step 4 : les tests du SDK à faire suivre**

Quatre tests écrivent ou lisent une ligne nue et passent à l'enveloppe :
`server.rs:975`, `server.rs:1029`, `client.rs:791`, `runtime.rs:254`. Le
commentaire de `metadata.rs:457` qui nomme `run_display_plugin` devient faux :
le corriger. Les quatre bouchons `impl DisplayPlugin` ne bougent pas — c'est
tout l'intérêt du corps par défaut, et un test le dit :

```rust
#[tokio::test]
async fn un_afficheur_qui_ignore_le_catalogue_recoit_quand_meme_les_etats() {
    // La propriete du corps par defaut : `console` n'a pas ete touche, et une
    // trame de catalogue ne doit ni le casser ni lui faire perdre la suivante.
    // ... ecrire une trame de catalogue PUIS une trame d'etat sur le socket ...
    assert_eq!(recus.lock().unwrap().len(), 1, "l'etat est passe malgre le catalogue");
}
```

- [ ] **Step 5 : lancer, et commit**

```
cargo test -p ritornello-proto -p ritornello-plugin-sdk
cargo clippy --workspace --all-targets -- -D warnings
git add -A
git commit -m "feat(proto,sdk): preselections nommees et deuxieme message du protocole daffichage"
```

---

## Task 11 : le cœur tient le catalogue

**Files:**
- Modify: `crates/ritornello-core/src/core.rs`
- Modify: `crates/ritornello-core/src/main.rs`

**Interfaces:**
- Produces: `Core::catalogue() -> Catalogue`, un `watch::Sender<Catalogue>`
  publié comme `etat_tx` l'est, `relais_afficheur` à quatre paramètres.

- [ ] **Step 1 : la table, lue avant le garde**

Dans `Core`, `presets_par_source: HashMap<String, Vec<Preset>>`. Dans
`handle_source_update`, les présélections sont lues **avant** le garde
`if self.standby || name != self.active_source { return; }` (`core.rs:309`), avec
la raison sur place :

```rust
        // Lu **avant** le garde ci-dessous, et c'est voulu : le catalogue décrit
        // toutes les sources, pas celle qui joue. Un client MPD interroge
        // `listplaylistinfo "radio"` pendant que le cd joue, et la veille ne
        // change rien à ce qu'une source contient. Le garde, lui, protège ce qui
        // décrit **ce qui joue** — identité, statut, éjection — et reste en
        // place pour tout le reste.
        if let Some(presets) = update.presets.take() {
            self.presets_par_source.insert(name.to_string(), presets);
            self.publie_catalogue();
        }
```

- [ ] **Step 2 : le canal et la publication**

`publie_catalogue` est le jumeau de `publie_etat` : il construit le `Catalogue`
depuis `source_order` (l'ordre de bascule, donc l'ordre que les clients verront)
et `presets_par_source`, et l'envoie par `send_if_modified` — même déduplication
par égalité, pour la même raison.

Il est appelé là où le catalogue peut changer, et **seulement** là : à l'arrivée
de présélections, et à `add_source` (une source câblée à chaud apparaît dans la
liste). Pas depuis `publie_etat` : les deux canaux sont séparés précisément pour
ne pas se déclencher l'un l'autre.

- [ ] **Step 3 : le relais à deux récepteurs**

`relais_afficheur` (`main.rs:72`) prend un `watch::Receiver<Catalogue>` de plus
et `select!` sur les deux, tel que la spec l'écrit. Ses **deux** sites d'appel
suivent : le démarrage (`main.rs:607`) et le câblage à chaud (`main.rs:219`), ce
dernier par `FilsChaud` qui gagne un champ `catalogue_rx`.

- [ ] **Step 4 : les demandes détachées**

Après le câblage des sources, une demande `ListPresets` par source, **dans une
tâche détachée**, telle que la spec l'écrit. Le commentaire doit dire pourquoi
elle est détachée : la réponse corrélée n'apprend rien, et attendre exposerait le
démarrage à 5 s par source injoignable — exactement les fenêtres que le chantier
précédent a supprimées.

- [ ] **Step 5 : les tests**

```rust
#[tokio::test]
async fn les_preselections_dune_source_inactive_sont_gardees() {
    // La raison d'etre du contournement du garde : `listplaylistinfo "radio"`
    // s'interroge pendant que le cd joue.
    // ... source active = "cd", puis un SourceUpdate de "radio" portant des presets ...
    let cat = core.catalogue();
    assert!(cat.sources.iter().any(|s| s.name == "radio" && !s.presets.is_empty()));
}

#[tokio::test]
async fn les_preselections_arrivent_meme_en_veille() {
    // Le garde arrete l'identite et le statut, pas un fait sur une source.
}

#[tokio::test]
async fn le_catalogue_suit_lordre_de_bascule_des_sources() {
    // C'est l'ordre que les clients verront dans `listplaylists`, et il doit
    // etre celui de `SourceCycle` : sinon la liste et la touche divergent.
    assert_eq!(noms(&core.catalogue()), core.source_order_pour_test());
}

#[tokio::test]
async fn une_source_cablee_a_chaud_entre_dans_le_catalogue() { }

#[tokio::test]
async fn le_catalogue_ne_republie_pas_pour_une_liste_identique() {
    // Meme deduplication que l'etat : une source qui reannonce la meme liste ne
    // doit pas reveiller les afficheurs.
}

#[tokio::test]
async fn publier_letat_ne_republie_pas_le_catalogue() {
    // La propriete des deux canaux separes. Sans elle, 51 noms voyageraient sur
    // chaque trame par seconde.
    let vu = catalogue_rx.borrow().clone();
    core.publie_etat();
    assert!(!catalogue_rx.has_changed().unwrap(), "le catalogue a bouge pour rien");
    let _ = vu;
}
```

- [ ] **Step 6 : lancer, et commit**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git add -A
git commit -m "feat(core): table des preselections par source et canal de catalogue vers les afficheurs"
```

---

## Task 12 : la radio énumère ses stations

**Files:**
- Modify: `crates/ritornello-plugin-radio/src/main.rs`
- Modify: `crates/ritornello-plugin-radio/src/config.rs`

- [ ] **Step 1 : le test, et le voir échouer**

```rust
#[test]
fn les_stations_senumerent_avec_leurs_noms_et_leurs_numeros() {
    let s: Stations = toml::from_str(r#"
        [[stations]]
        name = "FIP"
        url = "https://exemple/fip.mp3"
        preset = 1
        [[stations]]
        name = "Nova"
        url = "https://exemple/nova.mp3"
        preset = 5
    "#).unwrap();
    assert_eq!(s.presets(), vec![
        Preset { index: 1, name: "FIP".into() },
        Preset { index: 5, name: "Nova".into() },
    ]);
}

#[test]
fn lenumeration_est_triee_par_numero_pas_par_ordre_de_fichier() {
    // Les positions MPD suivront cet ordre : un stations.toml edite a la main
    // ne doit pas donner une liste en desordre chez le client.
}
```

- [ ] **Step 2 : `Stations::presets()` et la méthode du trait**

`presets()` dans `config.rs`, triée par `preset`. `list_presets` dans
`main.rs` la lit sous le `AsyncRwLock`.

- [ ] **Step 3 : la propagation spontanée**

La page d'admin de la radio pousse déjà `preset_count` quand elle enregistre
(`main.rs:180`, canal `preset_count_tx`). Faire voyager la liste par le même
chemin, pour qu'une station renommée se propage sans qu'on redemande. Un test :

```rust
#[tokio::test]
async fn enregistrer_les_stations_propage_la_nouvelle_liste() { }
```

- [ ] **Step 4 : le défaut reste le défaut**

Un test qui verrouille l'intention pour le cd :

```rust
// dans le SDK ou le crate du cd, selon ou vit le bouchon
#[tokio::test]
async fn une_source_qui_nenumere_pas_rend_une_liste_vide_sans_erreur() {
    // Le cd n'a que `total_tracks` et aucun nom : le defaut est la verite pour
    // lui, pas un manque a combler.
}
```

- [ ] **Step 5 : lancer, et commit**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git add -A
git commit -m "feat(radio): enumeration des stations nommees vers le catalogue"
```

---

## Task 13 : le greffon consomme le catalogue

**Files:**
- Modify: `crates/ritornello-plugin-mpd/src/etat.rs`, `commandes.rs`, `main.rs`

- [ ] **Step 1 : les tests, et les voir échouer**

```rust
#[test]
fn listplaylists_nomme_une_liste_par_source() {
    let inst = instantane_avec_catalogue(&["radio", "cd", "fichiers"]);
    let lignes = traiter_ok(&inst, &["listplaylists"]);
    assert_eq!(
        lignes.iter().filter(|l| l.starts_with("playlist: ")).count(),
        3
    );
    assert!(lignes.contains(&"playlist: radio".to_string()));
}

#[test]
fn listplaylistinfo_rend_les_vrais_noms() {
    let inst = instantane_avec_presets("radio", &[(1, "FIP"), (5, "Nova")]);
    let lignes = traiter_ok(&inst, &["listplaylistinfo", "radio"]);
    assert!(lignes.contains(&"Title: FIP".to_string()));
    assert!(lignes.contains(&"Title: Nova".to_string()));
}

#[test]
fn listplaylistinfo_interroge_une_source_qui_ne_joue_pas() {
    // Le cas qui a motive le contournement du garde cote coeur.
    let inst = instantane_actif_sur("cd", &[("radio", &[(1, "FIP")])]);
    assert!(traiter_ok(&inst, &["listplaylistinfo", "radio"]).contains(&"Title: FIP".to_string()));
}

#[test]
fn un_nom_de_liste_inconnu_est_un_ack_50() {
    let inst = instantane_avec_catalogue(&["radio"]);
    assert_eq!(
        traiter(&inst, 0, &["listplaylistinfo".into(), "nawak".into()]),
        Issue::Refuser("ACK [50@0] {listplaylistinfo} no such playlist".to_string())
    );
}

#[test]
fn load_bascule_de_source() {
    let inst = instantane_avec_catalogue(&["radio", "cd"]);
    assert_eq!(cmds(&inst, &["load", "cd"]), vec![Command::SelectSource("cd".into())]);
}

#[test]
fn load_dun_nom_inconnu_est_refuse_et_nemet_rien() {
    // Le greffon ne propose que des noms recus du catalogue : c'est lui qui
    // refuse, pas le coeur en silence.
    let inst = instantane_avec_catalogue(&["radio"]);
    assert!(matches!(traiter(&inst, 0, &["load".into(), "nawak".into()]), Issue::Refuser(_)));
}

#[tokio::test]
async fn le_catalogue_ne_voyage_pas_avec_chaque_trame_detat() {
    // Non-regression du choix des deux canaux : dix trames d'etat, un seul
    // catalogue.
    let e = EtatPartage::default();
    e.appliquer_catalogue(catalogue_a_une_source());
    let apres_catalogue = e.versions()[Sujet::StoredPlaylist as usize];
    for v in 0..10u8 {
        e.appliquer_etat(PlayerState { volume: v, ..Default::default() });
    }
    assert_eq!(e.versions()[Sujet::StoredPlaylist as usize], apres_catalogue);
}

#[tokio::test]
async fn un_catalogue_neuf_reveille_stored_playlist() { }
```

- [ ] **Step 2 : l'implémentation**

- `AfficheurMpd` implémente `catalogue()` : `etat.appliquer_catalogue(c)`.
- `appliquer_catalogue` incrémente `StoredPlaylist`, et **aussi** `Playlist` si
  les présélections de la source **active** ont changé — la file d'attente vient
  de là.
- `listplaylists` rend une ligne `playlist: <nom>` par source du catalogue, dans
  l'ordre reçu (celui de `SourceCycle`). MPD attend aussi un
  `Last-Modified:` par entrée ; le rendre à une constante `1970-01-01T00:00:00Z`
  avec le commentaire disant pourquoi : aucune date n'existe côté appareil, et
  omettre le champ fait trébucher certains clients.
- `listplaylistinfo <nom>` rend les entrées de cette source ; `ACK 50` si le nom
  n'est pas au catalogue.
- `load <nom>` remplace le `Refuser` provisoire de la Task 7 : `ACK 50` si
  inconnu, sinon `Command::SelectSource(nom)`.
- `COMMANDES` gagne `listplaylists`, `listplaylistinfo` et `load`.

- [ ] **Step 3 : lancer, et commit**

```
cargo test -p ritornello-plugin-mpd
cargo clippy --workspace --all-targets -- -D warnings
git add -A
git commit -m "feat(mpd): les sources vues comme listes enregistrees, et load qui bascule"
```

---

## Task 14 : la documentation

**Files:**
- Modify: `docs/plugins.md`
- Modify: `docs/interface.md` (seulement si l'état publié y est décrit)

- [ ] **Step 1 : le greffon**

Une section `## ritornello-plugin-mpd — l'appareil vu comme un serveur MPD`,
placée après `generic-input`. Elle doit dire, en anglais comme le reste du
fichier :

- ce que le client voit (les sources comme listes enregistrées, les
  présélections comme entrées) et ce qu'il ne voit pas (aucune base de données,
  aucune écriture), avec `commands` comme moyen pour lui de le découvrir ;
- l'écoute et l'absence de mot de passe, **avec sa conséquence** : quiconque est
  sur le réseau local commande l'appareil ;
- le port lié **avant** l'annonce, et ce que ça donne dans la page de statut
  quand 6600 est déjà pris ;
- la variable `RITORNELLO_MPD_CONFIG` et la page d'admin ;
- le décalage positions denses / indices creux, parce que c'est le piège qu'un
  futur lecteur du code doit trouver écrit ;
- que `pause 1` sur un état inconnu repose sur un état optimiste, et pourquoi
  aucune commande `SetPause` n'existe.

- [ ] **Step 2 : le deuxième message du protocole `display`**

La section `ritornello-plugin-console` annonce aujourd'hui qu'ajouter un message
au protocole d'affichage serait non cassant. C'est fait : décrire l'enveloppe
`DisplayFrame`, la trame de catalogue, le corps par défaut qui laisse `console`
indifférent, et **pourquoi le catalogue a son propre canal** plutôt qu'un champ
dans `PlayerState`. Corriger la phrase qui présentait cet ajout comme
hypothétique.

- [ ] **Step 3 : `ListPresets`**

Dans la partie qui décrit le protocole des sources : la requête, le corps par
défaut, le fait que la liste voyage **hors corrélation** comme `preset_count`, et
que seule la radio la sert — le cd et les fichiers n'ayant rien à nommer.

- [ ] **Step 4 : l'état publié**

`PlayerState.playback` est un champ neuf visible de la SPA : le mentionner là où
les autres champs le sont, avec la remarque que le bouton lecture/pause peut
désormais dire dans quel sens il va.

- [ ] **Step 5 : commit**

```
git add -A
git commit -m "docs: le greffon mpd, le deuxieme message daffichage et lenumeration des preselections"
```

---

## Auto-relecture du plan

**Couverture de la spec.** Les huit sections de la spec sont couvertes : le but
et le nœud n'appellent pas de code ; l'architecture est bâtie par les Tasks 3 à
8 ; « Ce que le greffon ne fait pas » est verrouillé par le test
`les_commandes_decriture_sont_refusees_une_par_une` (Task 6) ; le réseau par la
Task 3 ; le protocole MPD par les Tasks 4, 6, 7 et 8 ; les cinq additions par
les Tasks 1, 2, 10, 11 et 12 ; le greffon lui-même par les Tasks 3 à 9 et 13 ;
l'emballage par la Task 9 ; les tests sont dans chaque tâche ; « ce qui reste non
vérifié » est un constat, pas un travail.

**Cohérence des types.** `Preset` est défini une seule fois, dans `source.rs`
(Task 10), et `Catalogue` l'importe — le tableau « Où vivent les types neufs » de
la spec est la référence. `Playback` (Task 1) est employé par les Tasks 5, 6 et
7. `Issue` (Task 6) est étendu par la Task 7 et consommé par la Task 8.
`Entree` (Task 6) est distinct de `Preset` : le premier est la vue dense du
greffon, le second le fait creux de la source. `Instantane::playback()` rend
l'état optimiste, jamais le champ brut — c'est ce que les Tasks 6 et 7 lisent.

**Dépendances entre tâches.** 1 → 5, 6, 7 (a besoin de `Playback`). 2 → 13 (a
besoin de `SelectSource`). 3 → 4 → 5 → 6 → 7 → 8 → 9, en chaîne.
10 → 11 → 12 → 13. La Task 13 dépend aussi de la 7 (elle y remplace un
`Refuser` provisoire). La Task 14 est la dernière.

**Le jalon.** À la fin de la Task 9, le greffon est essayable au téléphone sans
les listes enregistrées : les onglets « bibliothèque » de M.A.L.P. seront vides
et sa file d'attente sera numérotée, mais lecture, pause, volume, piste suivante
et présélections fonctionnent. C'est l'ordre qui donne quelque chose à mettre
entre les mains le plus tôt.

## Le chantier des pochettes tourne en parallèle

`docs/superpowers/specs/2026-08-24-pochettes-album-design.md`, dans le worktree
`pochettes-album`, part du même `main` (`8931d96`). Rien à coordonner sur le
fond — les deux ajouts sont additifs et indépendants — mais **les conflits
textuels sont garantis**, et les voici nommés pour que la fusion soit mécanique
plutôt qu'une enquête :

| Endroit | Pochettes ajoute | Ce chantier ajoute |
|---|---|---|
| `proto/src/source.rs` (`SourceMessage`) | `cover: Option<CoverRef>` | `presets: Option<Vec<Preset>>` |
| `sdk/src/server.rs` (`Notification`) | le même `cover` | le même `presets` |
| `sdk/src/server.rs:291` et `:312` | estampille `cover` | estampille `presets` |
| `sdk/src/client.rs:75-80` | `\|\| msg.cover.is_some()` | `\|\| msg.presets.is_some()` |
| `sdk/src/client.rs:86` (`SourceUpdate`) | copie `cover` | copie `presets` |
| `proto/src/metadata.rs` | deux champs sur `Morceau` | `Playback` + un champ sur `PlayerState` |

Un piège d'ordre, moins visible : leur spec conclut « `PlayerState` — rien à
faire », parce que `Morceau` y est déjà aplati par `serde(flatten)`. Or **ce
chantier change la façon dont `PlayerState` voyage** — enveloppé dans une
`DisplayFrame` (Task 10). Les deux se composent sans heurt (le `data` de
l'enveloppe porte le `Morceau` enrichi, c'est précisément ce que l'étiquetage
adjacent garantit), mais **celui qui fusionne en second doit reprendre les tests
de l'autre** s'ils écrivent des lignes nues sur un socket d'affichage.

**Ce que les pochettes rendront possible, et qui n'est pas dans ce périmètre.**
Une fois `Morceau.cover_href` en place, MPD `albumart` et `readpicture`
deviennent atteignables — et c'est le visuel principal de M.A.L.P. Ils restent
dehors, pour une raison de conception et pas de temps : `cover_href` est une URL
du serveur HTTP **du cœur** (`/api/cover/{clé}`), or le greffon est un autre
processus. Les servir demanderait de lui donner un client HTTP et l'adresse du
cœur — un couplage qu'aucun greffon n'a aujourd'hui, et qui mérite sa propre
décision. `commands` n'annoncera donc pas ces deux commandes, ce qui suffit à ce
que les clients ne les demandent pas.

**Deux dettes assumées, écrites pour ne pas être prises pour des oublis.**
`load` rend un refus provisoire entre les Tasks 7 et 13, avec un commentaire qui
renvoie à la tâche. Et `stats` rend `uptime: 0` plutôt que de mémoriser un
instant de départ : aucun client n'en fait rien, et une horloge de plus est une
occasion de flake de plus.
</content>
