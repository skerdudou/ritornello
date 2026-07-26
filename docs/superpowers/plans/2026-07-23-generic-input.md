# Plugin `generic-input` configurable — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transformer `ritornello-plugin-mce` (un seul périphérique evdev, table de touches codée en dur) en `ritornello-plugin-generic-input` : tous les périphériques evdev lisibles écoutés simultanément, bindings persistés et modifiables depuis une page d'admin, presets livrés chargeables d'un clic, et mode apprentissage d'une touche depuis le navigateur — sans aucune extension du protocole d'admin.

**Architecture:** Plugin bicéphale à deux tâches `tokio::spawn` indépendantes, jointes sur leurs `JoinHandle` (modèle du plugin radio). La moitié **Input** ouvre tous les nœuds `/dev/input/event*` lisibles (non exclusif, pas d'`EVIOCGRAB`), une tâche de lecture par nœud, toutes alimentant un unique `mpsc<Command>` consommé par `run_input_plugin`. La moitié **Admin** implémente `AdminPlugin` (`page` / `get_data` / `set_data`) servie par le cœur sous l'origine unique. Les deux moitiés partagent un `Hub` (`Arc<std::sync::RwLock<…>>`) portant la table de bindings, l'état d'apprentissage et la carte des nœuds ouverts ; la résolution (nom du périphérique, code) → `Command` se fait **au moment de l'événement**, donc une modification de la table s'applique sans redémarrage. La persistance est un TOML atomique (tmp + `rename`) validé par une erreur typée traduite via le catalogue i18n.

**Tech Stack:** Rust 2021, tokio, evdev 0.12 (feature `tokio`), serde / serde_json / toml 0.8, `ritornello-proto` (`Command`), `ritornello-plugin-sdk` (`InputPlugin`, `run_input_plugin`, `AdminPlugin`, `run_admin_plugin`), `ritornello-i18n` (`Catalog`), tracing, tempfile (dev).

## Global Constraints

- Nom : `ritornello-plugin-generic-input`, déclaré `name = "generic-input"`, `kind = "input"`, `admin = true`.
- Périphériques écoutés : **tous** les périphériques evdev lisibles sont ouverts ; la table de bindings est consultée **au moment de l'événement**.
- Clé d'un binding : le **nom** du périphérique (stable au redémarrage), pas son chemin ; tous les nœuds portant ce nom sont écoutés.
- Persistance : `/etc/ritornello/input-bindings.toml` (surchargeable par `RITORNELLO_INPUT_BINDINGS`).
- Presets : fichiers livrés dans `/etc/ritornello/input-presets/*.toml` (depuis `deploy/input-presets/`, racine surchargeable par `RITORNELLO_INPUT_PRESETS`).
- Apprentissage : sans extension du protocole d'admin : opérations dans `SetData`, état lu par sondage de `GetData`.
- Rafraîchissement : bouton dédié : ré-énumère **et ouvre** les périphériques nouvellement détectés, sans recharger la page.
- Structure : plugin bicéphale (Input + Admin) en **deux tâches `tokio::spawn` indépendantes** (leçon acquise sur le plugin radio) — jamais de `try_join!`, jointure sur les `JoinHandle` et log de chaque fin.
- Hors périmètre : branchement à chaud automatique (udev), combinaisons de touches, appui long, axes/manettes.
- L'ouverture n'est **pas** exclusive (aucun `EVIOCGRAB`) ; toute touche non liée est ignorée silencieusement.
- Tout est best-effort : fichier absent/illisible → configuration vide + `warn`, jamais de panique ; périphérique illisible → ignoré et logué ; périphérique débranché → sa tâche se termine, les autres continuent.
- L'apprentissage supprime l'émission de commande **uniquement** pour le périphérique visé ; les autres continuent de fonctionner.
- Rien n'est persisté tant que l'utilisateur n'a pas cliqué « Enregistrer » (`op:"save"`) ; `load_preset` ne change que l'état en mémoire.
- Réutiliser `ritornello_proto::Command` pour les bindings — jamais de liste de commandes dupliquée.
- **Nom des tables TOML/JSON** : la spec donne un croquis Rust avec `rename = "device"`/`"binding"` mais un exemple JSON avec `"devices"`. La contrainte explicite étant la forme JSON de `get_data`, on retient **`devices` / `bindings` au pluriel, sans `rename`** (précédent `Stations { stations }` du plugin radio) : le TOML s'écrit donc `[[devices]]` / `[[devices.bindings]]`, le JSON `{"devices":[{"name":…,"bindings":[…]}]}`.
- Les modules qui atterrissent avant leur câblage (Tasks 2 à 6) sont déclarés `#[allow(dead_code)]` dans `main.rs` ; **Task 8 retire tous ces `allow`** (le workspace doit rester `clippy -D warnings` propre après chaque task).
- Le workspace doit compiler après **chaque** task ; une seule commit par task (dernière étape).
- Messages de commit et commentaires de code **en français** (convention du dépôt ; les sujets de commit restent sans accents comme l'historique existant).
- Tests unitaires en `#[cfg(test)] mod` **dans le fichier testé** (convention Rust déjà en place dans ce dépôt).
- Toute task qui change une dépendance ou un nom de crate commite le `Cargo.lock` régénéré.
- Aucune garde de verrou `std::sync::RwLock` ne traverse un `.await`.

---

## File Structure

- `crates/ritornello-plugin-mce/` → `crates/ritornello-plugin-generic-input/` (renommer — Task 1).
- `crates/ritornello-plugin-generic-input/Cargo.toml` (modifier) — nom, `[[bin]]`, dépendances serde/serde_json/toml/i18n, dev-dep tempfile.
- `crates/ritornello-plugin-generic-input/src/main.rs` (modifier) — deux moitiés `tokio::spawn`, état partagé, variables d'environnement.
- `crates/ritornello-plugin-generic-input/src/bindings.rs` (créer) — `Binding`/`Device`/`Bindings`, chargement, sauvegarde atomique, `validate`, `resolve`.
- `crates/ritornello-plugin-generic-input/src/presets.rs` (créer) — découverte et chargement des presets.
- `crates/ritornello-plugin-generic-input/src/devices.rs` (créer) — énumération, ouverture, boucles de lecture evdev, `Hub`.
- `crates/ritornello-plugin-generic-input/src/learn.rs` (créer) — machine à états de l'apprentissage (pure).
- `crates/ritornello-plugin-generic-input/src/admin.rs` (créer) — `AdminPlugin` (page, get_data, set_data).
- `crates/ritornello-plugin-generic-input/src/index.html` (créer) — gabarit à jetons `{{clé}}`.
- `crates/ritornello-plugin-generic-input/src/locales/en.toml` (créer) — anglais embarqué.
- `crates/ritornello-plugin-generic-input/src/input.rs` (supprimer — Task 8).
- `crates/ritornello-plugin-generic-input/src/keymap.rs` (supprimer — Task 8, sa table devient `deploy/input-presets/mce.toml`).
- `deploy/input-presets/mce.toml` (créer) — preset MCE.
- `deploy/input-presets/keyboard.toml` (créer) — preset clavier.
- `deploy/input-bindings.example.toml` (créer) — exemple de fichier de bindings.
- `deploy/locales/generic-input/fr.toml` (créer) — pack français.
- `Cargo.toml` (racine, modifier) — membre du workspace renommé.
- `Cargo.lock` (modifier) — régénéré aux tasks 1, 2.
- `deploy/plugins.example.toml` (modifier) — entrée `generic-input`, `admin = true`.
- `deploy/deploy.sh` (modifier) — binaire renommé, copie de `deploy/input-presets/`.
- `README.md` (modifier) — nom du plugin, page d'admin, variables d'environnement.

---

### Task 1: Renommer le crate en `ritornello-plugin-generic-input`

Renommage pur : le comportement (keymap codée en dur, `RITORNELLO_MCE_*`) est **inchangé** à ce stade, le workspace reste vert.

**Files:**
- Rename: `crates/ritornello-plugin-mce/` → `crates/ritornello-plugin-generic-input/`
- Modify: `crates/ritornello-plugin-generic-input/Cargo.toml`
- Modify: `Cargo.toml` (racine)
- Modify: `deploy/plugins.example.toml`
- Modify: `deploy/deploy.sh`
- Modify: `README.md`
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: `ritornello_plugin_sdk::{InputPlugin, run_input_plugin}`, `ritornello_proto::Command` (inchangés).
- Produces: binaire `ritornello-plugin-generic-input` (même comportement que `ritornello-plugin-mce`).

- [ ] **Step 1: Constater l'état de départ (test qui doit échouer après renommage)**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-mce"`
Expected : PASS (7 tests : 3 de `input.rs`, 3 de `keymap.rs`, 1 de `find_device`). C'est la référence à retrouver sous le nouveau nom.

- [ ] **Step 2: Renommer le répertoire**

Sous Git Bash (hôte Windows), depuis la racine du dépôt :

```bash
git mv crates/ritornello-plugin-mce crates/ritornello-plugin-generic-input
```

**Piège Windows :** `git mv` d'un répertoire échoue parfois avec « Permission denied » (antivirus/indexeur qui tient un handle sur `target/` ou sur un fichier source). Contournement documenté et utilisé dans ce dépôt :

```bash
cp -r crates/ritornello-plugin-mce crates/ritornello-plugin-generic-input
rm -rf crates/ritornello-plugin-mce
git add -A
```

Vérifier ensuite que git voit bien un renommage (`git status` doit montrer `renamed:` après `git add -A`).

- [ ] **Step 3: Renommer le crate et le binaire**

`crates/ritornello-plugin-generic-input/Cargo.toml` — contenu complet :

```toml
[package]
name = "ritornello-plugin-generic-input"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ritornello-plugin-generic-input"
path = "src/main.rs"

[dependencies]
anyhow = "1"
evdev = { version = "0.12", features = ["tokio"] }
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
async-trait = "0.1"
ritornello-proto = { path = "../ritornello-proto" }
ritornello-plugin-sdk = { path = "../ritornello-plugin-sdk" }
```

`Cargo.toml` (racine) — contenu complet :

```toml
[workspace]
resolver = "2"
members = [
    "crates/ritornello-proto",
    "crates/ritornello-i18n",
    "crates/ritornello-plugin-sdk",
    "crates/ritornello-core",
    "crates/ritornello-plugin-radio",
    "crates/ritornello-plugin-cd",
    "crates/ritornello-plugin-console",
    "crates/ritornello-plugin-generic-input",
]
```

- [ ] **Step 4: Lancer les tests sous le nouveau nom**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : les 7 tests passent, aucun warning clippy. (`cargo test -p ritornello-plugin-mce` doit désormais échouer avec `package ID specification ... did not match any packages` : c'est la preuve du renommage.)

- [ ] **Step 5: Déclaration du plugin (nom, admin) et déploiement**

`deploy/plugins.example.toml` — contenu complet :

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
name = "generic-input"
kind = "input"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-generic-input"
admin = true

[[plugin]]
name = "console"
kind = "display"
exec = "/usr/local/lib/ritornello/plugins/ritornello-plugin-console"
```

Note : `admin = true` est déclaré dès maintenant alors que la moitié admin
n'arrive qu'en Task 8. Sans conséquence pour les tests et le build ; sur un
déploiement réel intermédiaire, le cœur passerait un `--admin-socket` que le
binaire ignore (l'ancien `socket_path_from_args` cherche `--socket` par égalité
stricte, donc la moitié Input continue de fonctionner) et marquerait la page
d'admin injoignable (`502`) jusqu'à la Task 8.

`deploy/deploy.sh` — remplacer les deux lignes qui nomment le binaire :

```bash
scp "$OUT/ritornello-plugin-radio" "$OUT/ritornello-plugin-cd" "$OUT/ritornello-plugin-generic-input" "$OUT/ritornello-plugin-console" "$PI:/tmp/"
```

```bash
  && sudo mv /tmp/ritornello-plugin-radio /tmp/ritornello-plugin-cd /tmp/ritornello-plugin-generic-input /tmp/ritornello-plugin-console /usr/local/lib/ritornello/plugins/ \
```

- [ ] **Step 6: README — mentions du crate**

Dans `README.md` :
- section `## Plugins`, puce « Aucun de ces plugins n'est spécifique au Pi » : remplacer `ritornello-plugin-mce` par `ritornello-plugin-generic-input`.
- section `## Télécommande`, dernière phrase : remplacer `crates/ritornello-plugin-mce/src/keymap.rs` par `crates/ritornello-plugin-generic-input/src/keymap.rs` (ce paragraphe sera réécrit en Task 8).

- [ ] **Step 7: Vérifier qu'aucune référence à l'ancien nom ne subsiste**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn 'plugin-mce' --exclude-dir=target --exclude-dir=.git . || echo CLEAN"`
Expected : `CLEAN`.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test --workspace && cargo clippy --workspace -- -D warnings"`
Expected : tout vert (le `Cargo.lock` est régénéré au passage).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(input): renomme ritornello-plugin-mce en ritornello-plugin-generic-input"
```

---

### Task 2: `bindings.rs` — types, chargement, sauvegarde atomique, validation, résolution

**Files:**
- Create: `crates/ritornello-plugin-generic-input/src/bindings.rs`
- Create: `crates/ritornello-plugin-generic-input/src/locales/en.toml`
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`
- Modify: `crates/ritornello-plugin-generic-input/Cargo.toml`
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: `ritornello_proto::Command` (`#[serde(tag = "cmd", content = "arg")]`), `ritornello_i18n::Catalog` (`load`, `get`).
- Produces:
  - `pub struct Binding { pub code: u16, #[serde(flatten)] pub command: Command }`
  - `impl Binding { pub fn new(code: u16, command: &Command) -> Self; pub fn command(&self) -> Option<Command>; }`
  - `pub struct Device { pub name: String, #[serde(default)] pub bindings: Vec<Binding> }`
  - `pub struct Bindings { #[serde(default)] pub devices: Vec<Device> }`
  - `impl Bindings { pub fn load(path: &Path) -> Bindings; pub fn save(&self, path: &Path) -> anyhow::Result<()>; pub fn validate(&self) -> Result<(), ValidationError>; pub fn resolve(&self, device_name: &str, code: u16) -> Option<Command>; pub fn replace_device(&mut self, device: &str, bindings: Vec<Binding>); }`
  - `pub enum ValidationError { DuplicateCode { device: String, code: u16 }, SelectOutOfRange { device: String, arg: u8 }, UnknownCommand { device: String, code: u16 } }` avec `pub fn message(&self, catalog: &Catalog) -> String`.
  - `pub(crate) const GENERIC_INPUT_EN: &str` (dans `main.rs`).

- [ ] **Step 1: Ajouter les dépendances**

`crates/ritornello-plugin-generic-input/Cargo.toml` — contenu complet :

```toml
[package]
name = "ritornello-plugin-generic-input"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "ritornello-plugin-generic-input"
path = "src/main.rs"

[dependencies]
anyhow = "1"
async-trait = "0.1"
evdev = { version = "0.12", features = ["tokio"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = "0.3"
ritornello-i18n = { path = "../ritornello-i18n" }
ritornello-plugin-sdk = { path = "../ritornello-plugin-sdk" }
ritornello-proto = { path = "../ritornello-proto" }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Anglais embarqué (clés de validation)**

Créer `crates/ritornello-plugin-generic-input/src/locales/en.toml` (les clés de la page arrivent en Task 7) :

```toml
# messages de validation
duplicate_code = "code {code} is bound twice on {device}"
select_out_of_range = "preset {n} out of range 1-9 on {device}"
unknown_command = "unknown command bound to code {code} on {device}"
unknown_preset = "unknown preset: {preset}"
bad_request = "invalid request: {detail}"
```

Déclarer la constante dans `crates/ritornello-plugin-generic-input/src/main.rs`, juste après les `use` :

```rust
// Câblé pour de bon en Task 8 (réécriture de main.rs) ; d'ici là, seul le
// module bindings et ses tests s'en servent.
#[allow(dead_code)]
pub(crate) const GENERIC_INPUT_EN: &str = include_str!("locales/en.toml");
```

et ajouter la déclaration de module en tête du fichier, à côté de `mod input;` / `mod keymap;` :

```rust
// Module câblé en Task 8 : pour l'instant utilisé uniquement par ses tests.
#[allow(dead_code)]
mod bindings;
```

- [ ] **Step 3: Écrire `bindings.rs` avec ses tests — l'aller-retour TOML EN PREMIER**

Créer `crates/ritornello-plugin-generic-input/src/bindings.rs` :

```rust
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_proto::Command;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Une touche liée à une commande. Le couple `cmd`/`arg` est exactement la
/// représentation sérialisée de `Command` (`#[serde(tag = "cmd", content =
/// "arg")]`) aplatie dans le binding : aucune liste de commandes n'est
/// dupliquée, et le même objet transite tel quel en JSON vers l'IHM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub code: u16,
    #[serde(flatten)]
    pub command: Command,
}

impl Binding {
    pub fn new(code: u16, command: &Command) -> Self {
        Binding { code, command: command.clone() }
    }

    /// Commande portée par ce binding. `Option` parce que la forme de repli
    /// documentée dans la spec (`cmd: String` + `arg: Option<u8>`) peut porter
    /// une commande inconnue ; sous la forme aplatie nominale, c'est toujours
    /// `Some`. Tout le reste du crate passe par cet accesseur, ce qui confine
    /// le repli éventuel à ce fichier.
    pub fn command(&self) -> Option<Command> {
        Some(self.command.clone())
    }
}

/// Les bindings d'un périphérique, identifié par son **nom** (stable au
/// redémarrage) : tous les nœuds evdev portant ce nom sont concernés.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// Erreur de validation typée : le texte utilisateur est produit à la
/// frontière via `message(&Catalog)` (modèle du plugin radio). `Display`
/// fournit une version anglaise pour les journaux internes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    DuplicateCode { device: String, code: u16 },
    SelectOutOfRange { device: String, arg: u8 },
    UnknownCommand { device: String, code: u16 },
}

impl ValidationError {
    /// Message localisé remonté à l'utilisateur (corps du 422 côté admin).
    pub fn message(&self, catalog: &Catalog) -> String {
        match self {
            ValidationError::DuplicateCode { device, code } => catalog
                .get("duplicate_code")
                .replace("{code}", &code.to_string())
                .replace("{device}", device),
            ValidationError::SelectOutOfRange { device, arg } => catalog
                .get("select_out_of_range")
                .replace("{n}", &arg.to_string())
                .replace("{device}", device),
            ValidationError::UnknownCommand { device, code } => catalog
                .get("unknown_command")
                .replace("{code}", &code.to_string())
                .replace("{device}", device),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::DuplicateCode { device, code } => {
                write!(f, "code {code} bound twice on {device}")
            }
            ValidationError::SelectOutOfRange { device, arg } => {
                write!(f, "preset {arg} out of range 1-9 on {device}")
            }
            ValidationError::UnknownCommand { device, code } => {
                write!(f, "unknown command bound to code {code} on {device}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

impl Bindings {
    /// Charge la table. Best-effort : fichier absent ou TOML invalide donnent
    /// une table vide avec un `warn` — jamais de panique, le plugin démarre et
    /// l'utilisateur corrige depuis la page d'admin.
    pub fn load(path: &Path) -> Bindings {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "bindings {} illisibles ({e}) : demarrage sans binding, utiliser la page d'admin",
                    path.display()
                );
                return Bindings::default();
            }
        };
        match toml::from_str::<Bindings>(&text) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "bindings {} invalides ({e}) : demarrage sans binding, utiliser la page d'admin",
                    path.display()
                );
                Bindings::default()
            }
        }
    }

    /// Écriture atomique : fichier temporaire puis `rename`, jamais de fichier
    /// tronqué si l'alimentation saute au mauvais moment.
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        for dev in &self.devices {
            let mut vus = HashSet::new();
            for b in &dev.bindings {
                if !vus.insert(b.code) {
                    return Err(ValidationError::DuplicateCode {
                        device: dev.name.clone(),
                        code: b.code,
                    });
                }
                match b.command() {
                    None => {
                        return Err(ValidationError::UnknownCommand {
                            device: dev.name.clone(),
                            code: b.code,
                        })
                    }
                    Some(Command::Select(n)) if !(1..=9).contains(&n) => {
                        return Err(ValidationError::SelectOutOfRange {
                            device: dev.name.clone(),
                            arg: n,
                        })
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Résolution au moment de l'événement : (nom du périphérique, code) →
    /// commande. `None` = touche non liée, ignorée silencieusement.
    pub fn resolve(&self, device_name: &str, code: u16) -> Option<Command> {
        self.devices
            .iter()
            .find(|d| d.name == device_name)
            .and_then(|d| d.bindings.iter().find(|b| b.code == code))
            .and_then(|b| b.command())
    }

    /// Remplace l'intégralité des bindings d'un périphérique (création de
    /// l'entrée si elle n'existe pas). Utilisé par `load_preset`.
    pub fn replace_device(&mut self, device: &str, bindings: Vec<Binding>) {
        match self.devices.iter_mut().find(|d| d.name == device) {
            Some(d) => d.bindings = bindings,
            None => self.devices.push(Device { name: device.to_string(), bindings }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exemple() -> Bindings {
        Bindings {
            devices: vec![
                Device {
                    name: "eHome Infrared Transceiver".into(),
                    bindings: vec![
                        Binding::new(115, &Command::VolumeUp),
                        Binding::new(2, &Command::Select(1)),
                    ],
                },
                Device {
                    name: "USB Keyboard".into(),
                    bindings: vec![Binding::new(57, &Command::PlayPause)],
                },
            ],
        }
    }

    /// PREMIER test du chantier : `#[serde(flatten)]` sur un enum à tag
    /// adjacent est éprouvé en JSON dans ce projet, pas en TOML. S'il échoue,
    /// appliquer sans discussion le repli documenté dans la spec (champs
    /// `cmd: String` + `arg: Option<u8>` et conversions vers `Command`), qui
    /// garde exactement la même forme de fichier et de JSON.
    #[test]
    fn binding_roundtrip_toml() {
        let avec_arg = Binding::new(2, &Command::Select(1));
        let t = toml::to_string_pretty(&avec_arg).unwrap();
        assert!(t.contains("code = 2"), "TOML produit: {t}");
        assert!(t.contains("cmd = \"Select\""), "TOML produit: {t}");
        assert!(t.contains("arg = 1"), "TOML produit: {t}");
        assert_eq!(toml::from_str::<Binding>(&t).unwrap(), avec_arg);

        let sans_arg = Binding::new(115, &Command::VolumeUp);
        let t2 = toml::to_string_pretty(&sans_arg).unwrap();
        assert!(!t2.contains("arg"), "TOML produit: {t2}");
        assert_eq!(toml::from_str::<Binding>(&t2).unwrap(), sans_arg);
    }

    #[test]
    fn binding_json_porte_cmd_et_arg_a_plat() {
        assert_eq!(
            serde_json::to_value(Binding::new(2, &Command::Select(1))).unwrap(),
            serde_json::json!({ "code": 2, "cmd": "Select", "arg": 1 })
        );
        assert_eq!(
            serde_json::to_value(Binding::new(166, &Command::Stop)).unwrap(),
            serde_json::json!({ "code": 166, "cmd": "Stop" })
        );
        let b: Binding =
            serde_json::from_value(serde_json::json!({ "code": 9, "cmd": "Select", "arg": 8 }))
                .unwrap();
        assert_eq!(b.command(), Some(Command::Select(8)));
    }

    #[test]
    fn roundtrip_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        exemple().save(&path).unwrap();
        assert_eq!(Bindings::load(&path), exemple());
    }

    #[test]
    fn fichier_absent_donne_une_table_vide() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Bindings::load(&dir.path().join("absent.toml")), Bindings::default());
    }

    #[test]
    fn toml_invalide_donne_une_table_vide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("casse.toml");
        std::fs::write(&path, "ceci n'est pas = du toml [").unwrap();
        assert_eq!(Bindings::load(&path), Bindings::default());
    }

    #[test]
    fn save_ne_laisse_pas_de_fichier_temporaire() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        exemple().save(&path).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join("input-bindings.toml.tmp").exists());
    }

    #[test]
    fn validate_refuse_un_code_lie_deux_fois_sur_un_meme_peripherique() {
        let mut b = exemple();
        b.devices[0].bindings.push(Binding::new(115, &Command::Mute));
        assert_eq!(
            b.validate(),
            Err(ValidationError::DuplicateCode {
                device: "eHome Infrared Transceiver".into(),
                code: 115
            })
        );
    }

    #[test]
    fn validate_accepte_le_meme_code_sur_deux_peripheriques_differents() {
        let mut b = exemple();
        b.devices[1].bindings.push(Binding::new(115, &Command::VolumeUp));
        assert!(b.validate().is_ok());
    }

    #[test]
    fn validate_refuse_un_select_hors_bornes() {
        let mut b = exemple();
        b.devices[1].bindings.push(Binding::new(11, &Command::Select(0)));
        assert_eq!(
            b.validate(),
            Err(ValidationError::SelectOutOfRange { device: "USB Keyboard".into(), arg: 0 })
        );
        let mut b2 = exemple();
        b2.devices[1].bindings.push(Binding::new(11, &Command::Select(10)));
        assert_eq!(
            b2.validate(),
            Err(ValidationError::SelectOutOfRange { device: "USB Keyboard".into(), arg: 10 })
        );
    }

    #[test]
    fn save_refuse_une_table_invalide_et_necrit_rien() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input-bindings.toml");
        let mut b = exemple();
        b.devices[0].bindings.push(Binding::new(115, &Command::Mute));
        assert!(b.save(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn message_de_validation_utilise_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "duplicate_code = \"code {code} lie deux fois sur {device}\"\n",
        )
        .unwrap();
        let cat =
            Catalog::load("generic-input", "fr", dir.path(), crate::GENERIC_INPUT_EN);
        let err = ValidationError::DuplicateCode { device: "X".into(), code: 42 };
        assert_eq!(err.message(&cat), "code 42 lie deux fois sur X");
    }

    #[test]
    fn resolve_trouve_la_commande_du_bon_peripherique() {
        let b = exemple();
        assert_eq!(b.resolve("eHome Infrared Transceiver", 115), Some(Command::VolumeUp));
        assert_eq!(b.resolve("eHome Infrared Transceiver", 2), Some(Command::Select(1)));
        assert_eq!(b.resolve("USB Keyboard", 57), Some(Command::PlayPause));
        // code non lié sur ce périphérique
        assert_eq!(b.resolve("USB Keyboard", 115), None);
        // périphérique inconnu
        assert_eq!(b.resolve("Souris", 115), None);
    }

    #[test]
    fn replace_device_remplace_ou_cree_lentree() {
        let mut b = exemple();
        b.replace_device("USB Keyboard", vec![Binding::new(50, &Command::Mute)]);
        assert_eq!(b.devices[1].bindings, vec![Binding::new(50, &Command::Mute)]);
        b.replace_device("Nouveau", vec![Binding::new(1, &Command::Power)]);
        assert_eq!(b.devices.len(), 3);
        assert_eq!(b.devices[2].name, "Nouveau");
    }
}
```

- [ ] **Step 4: Lancer les tests — l'aller-retour TOML est le juge**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input"`
Expected : les 13 tests de `bindings` passent (plus les 7 existants).

**Si et seulement si `binding_roundtrip_toml` échoue**, appliquer le repli de la spec — il ne touche que `bindings.rs`, l'API (`Binding::new`, `Binding::command()`) et les formes TOML/JSON restant identiques. Remplacer la structure `Binding` et son `impl` par :

```rust
/// Forme de repli (voir spec) : `cmd`/`arg` explicites au lieu du `flatten`
/// d'un enum à tag adjacent, que TOML ne digère pas. Le fichier écrit et le
/// JSON échangé sont rigoureusement identiques à la forme aplatie.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub code: u16,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<u8>,
}

impl From<&Command> for (String, Option<u8>) {
    fn from(c: &Command) -> Self {
        match c {
            Command::Select(n) => ("Select".to_string(), Some(*n)),
            Command::Next => ("Next".to_string(), None),
            Command::Prev => ("Prev".to_string(), None),
            Command::NextTrack => ("NextTrack".to_string(), None),
            Command::PrevTrack => ("PrevTrack".to_string(), None),
            Command::VolumeUp => ("VolumeUp".to_string(), None),
            Command::VolumeDown => ("VolumeDown".to_string(), None),
            Command::Mute => ("Mute".to_string(), None),
            Command::SourceCycle => ("SourceCycle".to_string(), None),
            Command::PlayPause => ("PlayPause".to_string(), None),
            Command::Stop => ("Stop".to_string(), None),
            Command::Eject => ("Eject".to_string(), None),
            Command::Power => ("Power".to_string(), None),
        }
    }
}

impl TryFrom<&Binding> for Command {
    type Error = ();
    fn try_from(b: &Binding) -> Result<Self, Self::Error> {
        Ok(match b.cmd.as_str() {
            "Select" => Command::Select(b.arg.ok_or(())?),
            "Next" => Command::Next,
            "Prev" => Command::Prev,
            "NextTrack" => Command::NextTrack,
            "PrevTrack" => Command::PrevTrack,
            "VolumeUp" => Command::VolumeUp,
            "VolumeDown" => Command::VolumeDown,
            "Mute" => Command::Mute,
            "SourceCycle" => Command::SourceCycle,
            "PlayPause" => Command::PlayPause,
            "Stop" => Command::Stop,
            "Eject" => Command::Eject,
            "Power" => Command::Power,
            _ => return Err(()),
        })
    }
}

impl Binding {
    pub fn new(code: u16, command: &Command) -> Self {
        let (cmd, arg) = command.into();
        Binding { code, cmd, arg }
    }

    pub fn command(&self) -> Option<Command> {
        Command::try_from(self).ok()
    }
}
```

et ajouter ce test au module de tests (la branche `UnknownCommand` devient atteignable) :

```rust
    #[test]
    fn validate_refuse_une_commande_inconnue() {
        let b = Bindings {
            devices: vec![Device {
                name: "X".into(),
                bindings: vec![Binding { code: 1, cmd: "Inexistante".into(), arg: None }],
            }],
        };
        assert_eq!(
            b.validate(),
            Err(ValidationError::UnknownCommand { device: "X".into(), code: 1 })
        );
    }
```

Relancer alors la même commande de test et signaler le repli dans le rapport de la task.

- [ ] **Step 5: Clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : aucun warning (le `#[allow(dead_code)]` sur `mod bindings` couvre les fonctions pas encore câblées).

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-plugin-generic-input Cargo.lock
git commit -m "feat(generic-input): table de bindings (chargement, sauvegarde atomique, validation, resolution)"
```

---

### Task 3: `presets.rs` + presets livrés `mce.toml` et `keyboard.toml`

**Files:**
- Create: `crates/ritornello-plugin-generic-input/src/presets.rs`
- Create: `deploy/input-presets/mce.toml`
- Create: `deploy/input-presets/keyboard.toml`
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`

**Interfaces:**
- Consumes: `crate::bindings::Binding` (Task 2), `ritornello_i18n::Catalog`.
- Produces:
  - `pub fn parse_preset_names(entries: &[String]) -> Vec<String>` (pur : noms de fichiers → noms de presets, triés, dédoublonnés).
  - `pub fn list(root: &Path) -> Vec<String>` (I/O fine au-dessus du parse pur).
  - `pub fn load(root: &Path, name: &str) -> Result<Vec<Binding>, UnknownPreset>`.
  - `pub struct UnknownPreset(pub String)` avec `pub fn message(&self, catalog: &Catalog) -> String`.

- [ ] **Step 1: Déclarer le module**

Dans `crates/ritornello-plugin-generic-input/src/main.rs`, à côté des autres déclarations :

```rust
// Module câblé en Task 8 : pour l'instant utilisé uniquement par ses tests.
#[allow(dead_code)]
mod presets;
```

- [ ] **Step 2: Écrire `presets.rs` avec ses tests**

Créer `crates/ritornello-plugin-generic-input/src/presets.rs` :

```rust
use crate::bindings::Binding;
use ritornello_i18n::Catalog;
use serde::Deserialize;
use std::path::Path;

/// Un preset est une simple liste de bindings, sans nom de périphérique.
#[derive(Debug, Clone, Default, Deserialize)]
struct Preset {
    #[serde(default)]
    bindings: Vec<Binding>,
}

/// Preset introuvable, illisible ou invalide — un seul cas d'erreur côté
/// utilisateur : « ce preset n'existe pas ». Le détail part dans les journaux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPreset(pub String);

impl UnknownPreset {
    pub fn message(&self, catalog: &Catalog) -> String {
        catalog.get("unknown_preset").replace("{preset}", &self.0)
    }
}

impl std::fmt::Display for UnknownPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown preset: {}", self.0)
    }
}

impl std::error::Error for UnknownPreset {}

/// Un nom de preset est un identifiant simple : il vient du navigateur et sert
/// à construire un chemin, donc ni séparateur ni point (pas de `../`).
fn nom_valide(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Parse pur du listing d'un répertoire : ne garde que les `*.toml` au nom
/// valide, sans l'extension, triés et dédoublonnés. Séparé de l'accès disque
/// pour être testable (comme `audio_output::parse_device_list` du cœur).
pub fn parse_preset_names(entries: &[String]) -> Vec<String> {
    let mut noms: Vec<String> = entries
        .iter()
        .filter_map(|e| e.strip_suffix(".toml"))
        .filter(|n| nom_valide(n))
        .map(|n| n.to_string())
        .collect();
    noms.sort();
    noms.dedup();
    noms
}

/// Noms des presets disponibles. Répertoire absent ou illisible → liste vide.
pub fn list(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        tracing::warn!("repertoire de presets {} illisible : aucun preset", root.display());
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    parse_preset_names(&entries)
}

/// Charge les bindings d'un preset. Nom invalide, fichier absent ou TOML
/// illisible → `UnknownPreset` (avec un `warn` détaillant la vraie cause).
pub fn load(root: &Path, name: &str) -> Result<Vec<Binding>, UnknownPreset> {
    if !nom_valide(name) {
        tracing::warn!("nom de preset refuse: {name}");
        return Err(UnknownPreset(name.to_string()));
    }
    let path = root.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        tracing::warn!("preset {} illisible: {e}", path.display());
        UnknownPreset(name.to_string())
    })?;
    let preset: Preset = toml::from_str(&text).map_err(|e| {
        tracing::warn!("preset {} invalide: {e}", path.display());
        UnknownPreset(name.to_string())
    })?;
    Ok(preset.bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::Command;

    /// Racine des presets livrés dans le dépôt (`deploy/input-presets`).
    fn presets_livres() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/input-presets")
    }

    #[test]
    fn parse_preset_names_ne_garde_que_les_toml_valides() {
        let entries = vec![
            "mce.toml".to_string(),
            "keyboard.toml".to_string(),
            "README.md".to_string(),
            "..toml".to_string(),
            "../evasion.toml".to_string(),
            "mce.toml".to_string(),
        ];
        assert_eq!(parse_preset_names(&entries), vec!["keyboard", "mce"]);
    }

    #[test]
    fn list_decouvre_les_presets_dun_repertoire() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mce.toml"), "").unwrap();
        std::fs::write(dir.path().join("keyboard.toml"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert_eq!(list(dir.path()), vec!["keyboard", "mce"]);
    }

    #[test]
    fn list_repertoire_absent_donne_une_liste_vide() {
        assert!(list(Path::new("/nonexistent-presets-xyz")).is_empty());
    }

    #[test]
    fn load_lit_les_bindings_dun_preset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test.toml"),
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n\n[[bindings]]\ncode = 2\ncmd = \"Select\"\narg = 1\n",
        )
        .unwrap();
        let b = load(dir.path(), "test").unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].code, 115);
        assert_eq!(b[0].command(), Some(Command::VolumeUp));
        assert_eq!(b[1].command(), Some(Command::Select(1)));
    }

    #[test]
    fn load_preset_inconnu_renvoie_une_erreur() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), "absent"), Err(UnknownPreset("absent".into())));
    }

    #[test]
    fn load_refuse_un_nom_detourne() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path(), "../../etc/passwd").is_err());
        assert!(load(dir.path(), "").is_err());
    }

    #[test]
    fn message_de_preset_inconnu_utilise_le_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "unknown_preset = \"preset inconnu : {preset}\"\n",
        )
        .unwrap();
        let cat = Catalog::load("generic-input", "fr", dir.path(), crate::GENERIC_INPUT_EN);
        assert_eq!(UnknownPreset("zzz".into()).message(&cat), "preset inconnu : zzz");
    }

    #[test]
    fn les_presets_livres_se_chargent_et_sont_non_vides() {
        let root = presets_livres();
        assert_eq!(list(&root), vec!["keyboard", "mce"]);

        let mce = load(&root, "mce").unwrap();
        assert!(!mce.is_empty());
        assert_eq!(mce.iter().find(|b| b.code == 115).unwrap().command(), Some(Command::VolumeUp));
        assert_eq!(mce.iter().find(|b| b.code == 513).unwrap().command(), Some(Command::Select(1)));
        assert_eq!(mce.iter().find(|b| b.code == 356).unwrap().command(), Some(Command::Power));

        let kbd = load(&root, "keyboard").unwrap();
        assert!(!kbd.is_empty());
        assert_eq!(kbd.iter().find(|b| b.code == 57).unwrap().command(), Some(Command::PlayPause));
        assert_eq!(kbd.iter().find(|b| b.code == 103).unwrap().command(), Some(Command::VolumeUp));
    }
}
```

- [ ] **Step 3: Lancer les tests — ils doivent échouer (presets livrés absents)**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input presets"`
Expected : FAIL sur `les_presets_livres_se_chargent_et_sont_non_vides` (répertoire `deploy/input-presets` inexistant) ; les autres tests de `presets` passent.

- [ ] **Step 4: Écrire le preset `mce.toml` (transcription exacte de `keymap.rs`)**

Créer `deploy/input-presets/mce.toml` :

```toml
# Telecommande MCE (recepteur infrarouge eHome et compatibles).
# Transcription de l'ancienne table codee en dur (keymap.rs) :
# chiffres 2-10 et 513-521 -> Select(1..9).

[[bindings]]
code = 2
cmd = "Select"
arg = 1

[[bindings]]
code = 3
cmd = "Select"
arg = 2

[[bindings]]
code = 4
cmd = "Select"
arg = 3

[[bindings]]
code = 5
cmd = "Select"
arg = 4

[[bindings]]
code = 6
cmd = "Select"
arg = 5

[[bindings]]
code = 7
cmd = "Select"
arg = 6

[[bindings]]
code = 8
cmd = "Select"
arg = 7

[[bindings]]
code = 9
cmd = "Select"
arg = 8

[[bindings]]
code = 10
cmd = "Select"
arg = 9

[[bindings]]
code = 513
cmd = "Select"
arg = 1

[[bindings]]
code = 514
cmd = "Select"
arg = 2

[[bindings]]
code = 515
cmd = "Select"
arg = 3

[[bindings]]
code = 516
cmd = "Select"
arg = 4

[[bindings]]
code = 517
cmd = "Select"
arg = 5

[[bindings]]
code = 518
cmd = "Select"
arg = 6

[[bindings]]
code = 519
cmd = "Select"
arg = 7

[[bindings]]
code = 520
cmd = "Select"
arg = 8

[[bindings]]
code = 521
cmd = "Select"
arg = 9

[[bindings]]
code = 115
cmd = "VolumeUp"

[[bindings]]
code = 114
cmd = "VolumeDown"

[[bindings]]
code = 113
cmd = "Mute"

[[bindings]]
code = 402
cmd = "Next"

[[bindings]]
code = 403
cmd = "Prev"

[[bindings]]
code = 164
cmd = "PlayPause"

[[bindings]]
code = 163
cmd = "NextTrack"

[[bindings]]
code = 165
cmd = "PrevTrack"

[[bindings]]
code = 166
cmd = "Stop"

[[bindings]]
code = 161
cmd = "Eject"

[[bindings]]
code = 226
cmd = "SourceCycle"

[[bindings]]
code = 116
cmd = "Power"

[[bindings]]
code = 356
cmd = "Power"
```

- [ ] **Step 5: Écrire le preset `keyboard.toml`**

Créer `deploy/input-presets/keyboard.toml` :

```toml
# Clavier ordinaire : chiffres 1-9 (codes 2-10) vers les preselections,
# fleches haut/bas pour le volume, fleches droite/gauche pour les
# preselections, espace lecture/pause, m muet, s changement de source,
# p veille.

[[bindings]]
code = 2
cmd = "Select"
arg = 1

[[bindings]]
code = 3
cmd = "Select"
arg = 2

[[bindings]]
code = 4
cmd = "Select"
arg = 3

[[bindings]]
code = 5
cmd = "Select"
arg = 4

[[bindings]]
code = 6
cmd = "Select"
arg = 5

[[bindings]]
code = 7
cmd = "Select"
arg = 6

[[bindings]]
code = 8
cmd = "Select"
arg = 7

[[bindings]]
code = 9
cmd = "Select"
arg = 8

[[bindings]]
code = 10
cmd = "Select"
arg = 9

[[bindings]]
code = 103
cmd = "VolumeUp"

[[bindings]]
code = 108
cmd = "VolumeDown"

[[bindings]]
code = 106
cmd = "Next"

[[bindings]]
code = 105
cmd = "Prev"

[[bindings]]
code = 57
cmd = "PlayPause"

[[bindings]]
code = 50
cmd = "Mute"

[[bindings]]
code = 31
cmd = "SourceCycle"

[[bindings]]
code = 25
cmd = "Power"
```

- [ ] **Step 6: Relancer les tests et clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : PASS pour les 8 tests de `presets` (dont celui des presets livrés) ; aucun warning clippy.

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-plugin-generic-input deploy/input-presets
git commit -m "feat(generic-input): presets (decouverte, chargement) et presets livres mce/keyboard"
```

---

### Task 4: `devices.rs` — énumération, ouverture de tous les nœuds, boucles de lecture

**Files:**
- Create: `crates/ritornello-plugin-generic-input/src/devices.rs`
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`

**Interfaces:**
- Consumes: `crate::bindings::Bindings` (Task 2), `evdev::{Device, EventType}`, `tokio::sync::mpsc::Sender<Command>`.
- Produces:
  - `pub const INPUT_DIR: &str = "/dev/input"`
  - `pub fn event_nodes(root: &Path, entries: &[String]) -> Vec<PathBuf>` (pur)
  - `pub fn scan_event_nodes(root: &Path) -> Vec<PathBuf>`
  - `pub fn key_outcome(bindings: &Bindings, learning_device: Option<&str>, device_name: &str, code: u16) -> Option<Command>` (pur)
  - `pub struct Hub { pub bindings: Arc<RwLock<Bindings>>, pub open: Arc<RwLock<BTreeMap<PathBuf, String>>>, pub tx: mpsc::Sender<Command> }` (`Clone`), avec `pub fn new(bindings: Bindings, tx: mpsc::Sender<Command>) -> Hub`, `pub fn device_names(&self) -> Vec<String>`, `pub fn open_new_devices(&self, root: &Path) -> usize`.

- [ ] **Step 1: Déclarer le module**

Dans `crates/ritornello-plugin-generic-input/src/main.rs` :

```rust
// Module câblé en Task 8 : pour l'instant utilisé uniquement par ses tests.
#[allow(dead_code)]
mod devices;
```

- [ ] **Step 2: Écrire `devices.rs` avec ses tests**

Créer `crates/ritornello-plugin-generic-input/src/devices.rs` :

```rust
use crate::bindings::Bindings;
use evdev::{Device, EventType};
use ritornello_proto::Command;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// Racine des nœuds evdev sur un Linux standard.
pub const INPUT_DIR: &str = "/dev/input";

/// Filtre pur d'un listing de répertoire : ne garde que les nœuds `eventN`,
/// triés. Séparé de l'accès disque pour être testable sans matériel (comme
/// `audio_output::parse_device_list` du cœur).
pub fn event_nodes(root: &Path, entries: &[String]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = entries
        .iter()
        .filter(|n| {
            n.strip_prefix("event")
                .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|n| root.join(n))
        .collect();
    v.sort();
    v
}

/// Listing disque des nœuds evdev. Répertoire absent ou illisible → liste
/// vide et `warn` : jamais fatal.
pub fn scan_event_nodes(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        tracing::warn!("repertoire {} illisible : aucun peripherique d'entree", root.display());
        return Vec::new();
    };
    let entries: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    event_nodes(root, &entries)
}

/// Ce que produit un appui de touche : la commande liée, ou rien. Le
/// périphérique en cours d'apprentissage n'émet plus rien (sinon apprendre
/// « Volume + » déclencherait un volume +) ; les autres continuent
/// normalement. Fonction pure, testable sans matériel.
pub fn key_outcome(
    bindings: &Bindings,
    learning_device: Option<&str>,
    device_name: &str,
    code: u16,
) -> Option<Command> {
    if learning_device == Some(device_name) {
        return None;
    }
    bindings.resolve(device_name, code)
}

/// État partagé entre la moitié Input (les tâches de lecture) et la moitié
/// Admin. `std::sync::RwLock` : les gardes sont toujours relâchées avant le
/// moindre `.await`, et `page()` (synchrone) peut lire sans runtime.
#[derive(Clone)]
pub struct Hub {
    pub bindings: Arc<RwLock<Bindings>>,
    /// Nœuds actuellement ouverts : chemin → nom du périphérique.
    pub open: Arc<RwLock<BTreeMap<PathBuf, String>>>,
    pub tx: mpsc::Sender<Command>,
}

impl Hub {
    pub fn new(bindings: Bindings, tx: mpsc::Sender<Command>) -> Hub {
        Hub {
            bindings: Arc::new(RwLock::new(bindings)),
            open: Arc::new(RwLock::new(BTreeMap::new())),
            tx,
        }
    }

    /// Noms des périphériques actuellement ouverts, triés et dédoublonnés
    /// (plusieurs nœuds peuvent porter le même nom).
    pub fn device_names(&self) -> Vec<String> {
        let mut noms: Vec<String> = self.open.read().unwrap().values().cloned().collect();
        noms.sort();
        noms.dedup();
        noms
    }

    /// Ouvre tous les nœuds evdev lisibles pas encore ouverts et lance une
    /// tâche de lecture par nœud. Renvoie le nombre de nouveaux nœuds. Un
    /// périphérique illisible (droits, disparu entre l'énumération et
    /// l'ouverture) est logué en `warn` et ignoré — jamais fatal.
    pub fn open_new_devices(&self, root: &Path) -> usize {
        let mut nouveaux = 0;
        for path in scan_event_nodes(root) {
            if self.open.read().unwrap().contains_key(&path) {
                continue;
            }
            let dev = match Device::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("peripherique {} illisible, ignore: {e}", path.display());
                    continue;
                }
            };
            let name = dev.name().unwrap_or("?").to_string();
            self.open.write().unwrap().insert(path.clone(), name.clone());
            self.spawn_reader(path, dev, name);
            nouveaux += 1;
        }
        nouveaux
    }

    /// Une tâche de lecture par nœud, toutes alimentant le même mpsc.
    fn spawn_reader(&self, path: PathBuf, dev: Device, name: String) {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut stream = match dev.into_event_stream() {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("flux evdev {} indisponible: {e}", path.display());
                    hub.forget(&path);
                    return;
                }
            };
            tracing::info!("peripherique ecoute: {name} ({})", path.display());
            loop {
                let ev = match stream.next_event().await {
                    Ok(ev) => ev,
                    Err(e) => {
                        // Débranchement : cette tâche se termine, les autres
                        // continuent.
                        tracing::info!("lecture de {} terminee: {e}", path.display());
                        break;
                    }
                };
                if ev.event_type() != EventType::KEY || ev.value() != 1 {
                    continue;
                }
                // Aucune garde de verrou ne traverse le `.await` d'envoi.
                let cmd = {
                    let b = hub.bindings.read().unwrap();
                    // L'apprentissage est câblé en Task 5.
                    key_outcome(&b, None, &name, ev.code())
                };
                if let Some(cmd) = cmd {
                    tracing::debug!("{name}: touche {} -> {cmd:?}", ev.code());
                    let _ = hub.tx.send(cmd).await;
                }
            }
            hub.forget(&path);
        });
    }

    /// Oublie un nœud dont la lecture s'est terminée.
    fn forget(&self, path: &Path) {
        self.open.write().unwrap().remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Device as BindDevice};

    fn table() -> Bindings {
        Bindings {
            devices: vec![BindDevice {
                name: "eHome".into(),
                bindings: vec![Binding::new(115, &Command::VolumeUp)],
            }],
        }
    }

    fn hub_de_test() -> (Hub, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel(8);
        (Hub::new(table(), tx), rx)
    }

    #[test]
    fn event_nodes_ne_garde_que_les_noeuds_event() {
        let entries = vec![
            "event10".to_string(),
            "event2".to_string(),
            "mice".to_string(),
            "by-id".to_string(),
            "eventX".to_string(),
            "event".to_string(),
        ];
        assert_eq!(
            event_nodes(Path::new("/dev/input"), &entries),
            vec![PathBuf::from("/dev/input/event10"), PathBuf::from("/dev/input/event2")]
        );
    }

    #[test]
    fn scan_event_nodes_repertoire_absent_donne_vide() {
        assert!(scan_event_nodes(Path::new("/nonexistent-input-xyz")).is_empty());
    }

    #[test]
    fn key_outcome_resout_le_binding_du_bon_peripherique() {
        let t = table();
        assert_eq!(key_outcome(&t, None, "eHome", 115), Some(Command::VolumeUp));
        assert_eq!(key_outcome(&t, None, "eHome", 42), None);
        assert_eq!(key_outcome(&t, None, "Autre", 115), None);
    }

    #[test]
    fn key_outcome_supprime_lemission_du_seul_peripherique_en_apprentissage() {
        let mut t = table();
        t.devices.push(BindDevice {
            name: "USB Keyboard".into(),
            bindings: vec![Binding::new(115, &Command::VolumeUp)],
        });
        // apprentissage sur eHome : eHome muet, le clavier continue
        assert_eq!(key_outcome(&t, Some("eHome"), "eHome", 115), None);
        assert_eq!(
            key_outcome(&t, Some("eHome"), "USB Keyboard", 115),
            Some(Command::VolumeUp)
        );
    }

    #[test]
    fn device_names_dedoublonne_et_trie() {
        let (hub, _rx) = hub_de_test();
        {
            let mut open = hub.open.write().unwrap();
            open.insert(PathBuf::from("/dev/input/event3"), "eHome".into());
            open.insert(PathBuf::from("/dev/input/event1"), "USB Keyboard".into());
            open.insert(PathBuf::from("/dev/input/event2"), "eHome".into());
        }
        assert_eq!(hub.device_names(), vec!["USB Keyboard", "eHome"]);
    }

    #[tokio::test]
    async fn open_new_devices_sur_un_repertoire_sans_noeud_nouvre_rien() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mice"), "").unwrap();
        let (hub, _rx) = hub_de_test();
        assert_eq!(hub.open_new_devices(dir.path()), 0);
        assert!(hub.device_names().is_empty());
    }

    #[test]
    fn forget_retire_le_noeud_de_la_carte() {
        let (hub, _rx) = hub_de_test();
        let p = PathBuf::from("/dev/input/event7");
        hub.open.write().unwrap().insert(p.clone(), "eHome".into());
        hub.forget(&p);
        assert!(hub.device_names().is_empty());
    }
}
```

- [ ] **Step 3: Lancer les tests et clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : les 7 tests de `devices` passent (plus les précédents) ; aucun warning clippy.

- [ ] **Step 4: Commit**

```bash
git add crates/ritornello-plugin-generic-input
git commit -m "feat(generic-input): ouverture de tous les peripheriques evdev, une tache de lecture par noeud"
```

---

### Task 5: `learn.rs` — machine à états de l'apprentissage (pure) et câblage dans le Hub

**Files:**
- Create: `crates/ritornello-plugin-generic-input/src/learn.rs`
- Modify: `crates/ritornello-plugin-generic-input/src/devices.rs`
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`

**Interfaces:**
- Consumes: `crate::devices::{Hub, key_outcome}` (Task 4).
- Produces:
  - `pub struct Learning { pub device: String, pub captured: Option<u16> }` (`Serialize`)
  - `pub struct LearnState { … }` (`Default`) avec `pub fn learn(&mut self, device: &str)`, `pub fn cancel(&mut self)`, `pub fn cancel_if(&mut self, device: &str)`, `pub fn device(&self) -> Option<&str>`, `pub fn capture(&mut self, device: &str, code: u16) -> bool`, `pub fn snapshot(&self) -> Option<Learning>`.
  - `Hub` gagne le champ `pub learn: Arc<RwLock<LearnState>>`.

- [ ] **Step 1: Déclarer le module**

Dans `crates/ritornello-plugin-generic-input/src/main.rs` :

```rust
// Module câblé en Task 8 : pour l'instant utilisé uniquement par ses tests.
#[allow(dead_code)]
mod learn;
```

- [ ] **Step 2: Écrire `learn.rs` avec ses tests**

Créer `crates/ritornello-plugin-generic-input/src/learn.rs` :

```rust
use serde::Serialize;

/// État d'apprentissage tel que l'IHM le lit dans `GetData` : `captured` reste
/// `null` tant qu'aucune touche n'a été pressée.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Learning {
    pub device: String,
    pub captured: Option<u16>,
}

/// Machine à états de l'apprentissage. Pure : aucune I/O, entièrement
/// testable sans matériel. L'apprentissage est **exclusif** — une nouvelle
/// demande remplace la précédente.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LearnState {
    current: Option<Learning>,
}

impl LearnState {
    /// Entre (ou ré-entre) en apprentissage pour ce périphérique.
    pub fn learn(&mut self, device: &str) {
        self.current = Some(Learning { device: device.to_string(), captured: None });
    }

    /// Sort de l'apprentissage sans rien retenir.
    pub fn cancel(&mut self) {
        self.current = None;
    }

    /// Abandonne l'apprentissage s'il visait ce périphérique (utilisé quand le
    /// périphérique disparaît).
    pub fn cancel_if(&mut self, device: &str) {
        if self.current.as_ref().is_some_and(|l| l.device == device) {
            self.current = None;
        }
    }

    /// Périphérique dont les événements doivent être **supprimés**, c'est-à-dire
    /// celui en apprentissage tant qu'aucun code n'a été capturé. Une fois le
    /// code capturé l'apprentissage est terminé : le périphérique réémet.
    pub fn device(&self) -> Option<&str> {
        match &self.current {
            Some(l) if l.captured.is_none() => Some(l.device.as_str()),
            _ => None,
        }
    }

    /// Retient le premier code pressé sur le périphérique visé. Renvoie `true`
    /// si l'événement a été consommé par l'apprentissage.
    pub fn capture(&mut self, device: &str, code: u16) -> bool {
        match &mut self.current {
            Some(l) if l.device == device && l.captured.is_none() => {
                l.captured = Some(code);
                tracing::info!("apprentissage: {device} -> code {code}");
                true
            }
            _ => false,
        }
    }

    /// Copie de l'état pour `GetData` (`None` hors apprentissage).
    pub fn snapshot(&self) -> Option<Learning> {
        self.current.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learn_puis_capture_retient_le_premier_code() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        assert_eq!(s.device(), Some("USB Keyboard"));
        assert_eq!(s.snapshot(), Some(Learning { device: "USB Keyboard".into(), captured: None }));
        assert!(s.capture("USB Keyboard", 115));
        assert_eq!(
            s.snapshot(),
            Some(Learning { device: "USB Keyboard".into(), captured: Some(115) })
        );
        // deuxième appui : plus rien à capturer, l'apprentissage est terminé
        assert!(!s.capture("USB Keyboard", 42));
        assert_eq!(s.snapshot().unwrap().captured, Some(115));
        // et le périphérique réémet ses commandes
        assert_eq!(s.device(), None);
    }

    #[test]
    fn capture_ignore_les_autres_peripheriques() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        assert!(!s.capture("eHome", 115));
        assert_eq!(s.snapshot().unwrap().captured, None);
    }

    #[test]
    fn capture_sans_apprentissage_ne_fait_rien() {
        let mut s = LearnState::default();
        assert!(!s.capture("USB Keyboard", 115));
        assert_eq!(s.snapshot(), None);
    }

    #[test]
    fn cancel_efface_letat() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.cancel();
        assert_eq!(s.snapshot(), None);
        assert_eq!(s.device(), None);
    }

    #[test]
    fn un_nouveau_learn_remplace_le_precedent() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.capture("USB Keyboard", 115);
        s.learn("eHome");
        assert_eq!(s.snapshot(), Some(Learning { device: "eHome".into(), captured: None }));
        assert_eq!(s.device(), Some("eHome"));
    }

    #[test]
    fn cancel_if_nabandonne_que_le_peripherique_vise() {
        let mut s = LearnState::default();
        s.learn("USB Keyboard");
        s.cancel_if("eHome");
        assert_eq!(s.device(), Some("USB Keyboard"));
        s.cancel_if("USB Keyboard");
        assert_eq!(s.snapshot(), None);
    }

    #[test]
    fn snapshot_se_serialise_comme_attendu() {
        let mut s = LearnState::default();
        assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), serde_json::Value::Null);
        s.learn("USB Keyboard");
        assert_eq!(
            serde_json::to_value(s.snapshot()).unwrap(),
            serde_json::json!({ "device": "USB Keyboard", "captured": null })
        );
        s.capture("USB Keyboard", 115);
        assert_eq!(
            serde_json::to_value(s.snapshot()).unwrap(),
            serde_json::json!({ "device": "USB Keyboard", "captured": 115 })
        );
    }
}
```

- [ ] **Step 3: Lancer les tests du module — ils doivent passer, le Hub n'est pas encore câblé**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input learn"`
Expected : les 7 tests de `learn` passent.

- [ ] **Step 4: Câbler l'apprentissage dans le Hub (test d'intégration d'abord)**

Ajouter ces deux tests à la fin du module `tests` de `crates/ritornello-plugin-generic-input/src/devices.rs` :

```rust
    #[test]
    fn le_hub_supprime_lemission_du_peripherique_en_apprentissage() {
        let (hub, _rx) = hub_de_test();
        hub.bindings.write().unwrap().devices.push(BindDevice {
            name: "USB Keyboard".into(),
            bindings: vec![Binding::new(115, &Command::VolumeUp)],
        });
        hub.learn.write().unwrap().learn("eHome");

        let sortie = |nom: &str, code: u16| {
            let learn = hub.learn.read().unwrap();
            let b = hub.bindings.read().unwrap();
            key_outcome(&b, learn.device(), nom, code)
        };
        assert_eq!(sortie("eHome", 115), None);
        assert_eq!(sortie("USB Keyboard", 115), Some(Command::VolumeUp));

        // une fois le code capturé, eHome réémet
        hub.learn.write().unwrap().capture("eHome", 115);
        assert_eq!(sortie("eHome", 115), Some(Command::VolumeUp));
    }

    #[test]
    fn forget_abandonne_lapprentissage_quand_le_dernier_noeud_disparait() {
        let (hub, _rx) = hub_de_test();
        let p1 = PathBuf::from("/dev/input/event1");
        let p2 = PathBuf::from("/dev/input/event2");
        {
            let mut open = hub.open.write().unwrap();
            open.insert(p1.clone(), "eHome".into());
            open.insert(p2.clone(), "eHome".into());
        }
        hub.learn.write().unwrap().learn("eHome");
        // un seul des deux nœuds disparaît : l'apprentissage continue
        hub.forget(&p1);
        assert_eq!(hub.learn.read().unwrap().device(), Some("eHome"));
        // le dernier disparaît : l'apprentissage est abandonné
        hub.forget(&p2);
        assert_eq!(hub.learn.read().unwrap().snapshot(), None);
    }
```

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input devices"`
Expected : FAIL à la compilation (`no field \`learn\` on type \`Hub\``).

- [ ] **Step 5: Ajouter le champ `learn` au Hub et l'utiliser dans la boucle de lecture**

Dans `crates/ritornello-plugin-generic-input/src/devices.rs` :

1. Ajouter l'import : `use crate::learn::LearnState;`
2. Structure `Hub` :

```rust
#[derive(Clone)]
pub struct Hub {
    pub bindings: Arc<RwLock<Bindings>>,
    pub learn: Arc<RwLock<LearnState>>,
    /// Nœuds actuellement ouverts : chemin → nom du périphérique.
    pub open: Arc<RwLock<BTreeMap<PathBuf, String>>>,
    pub tx: mpsc::Sender<Command>,
}
```

3. `Hub::new` :

```rust
    pub fn new(bindings: Bindings, tx: mpsc::Sender<Command>) -> Hub {
        Hub {
            bindings: Arc::new(RwLock::new(bindings)),
            learn: Arc::new(RwLock::new(LearnState::default())),
            open: Arc::new(RwLock::new(BTreeMap::new())),
            tx,
        }
    }
```

4. Dans `spawn_reader`, remplacer le bloc qui suit le filtre `EventType::KEY` par :

```rust
                // L'apprentissage consomme le premier appui et n'émet rien.
                let capture = { hub.learn.write().unwrap().capture(&name, ev.code()) };
                if capture {
                    continue;
                }
                // Aucune garde de verrou ne traverse le `.await` d'envoi.
                let cmd = {
                    let learn = hub.learn.read().unwrap();
                    let b = hub.bindings.read().unwrap();
                    key_outcome(&b, learn.device(), &name, ev.code())
                };
                if let Some(cmd) = cmd {
                    tracing::debug!("{name}: touche {} -> {cmd:?}", ev.code());
                    let _ = hub.tx.send(cmd).await;
                }
```

5. `forget` abandonne l'apprentissage quand plus aucun nœud ne porte le nom :

```rust
    /// Oublie un nœud dont la lecture s'est terminée. Si plus aucun nœud ne
    /// porte ce nom, l'apprentissage éventuellement en cours dessus est
    /// abandonné (le périphérique a disparu).
    fn forget(&self, path: &Path) {
        let nom = self.open.write().unwrap().remove(path);
        if let Some(nom) = nom {
            if !self.device_names().contains(&nom) {
                self.learn.write().unwrap().cancel_if(&nom);
            }
        }
    }
```

- [ ] **Step 6: Relancer les tests et clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : tous les tests passent (9 dans `devices`, 7 dans `learn`) ; aucun warning clippy.

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-plugin-generic-input
git commit -m "feat(generic-input): mode apprentissage (machine a etats pure, suppression ciblee de l'emission)"
```

---

### Task 6: `admin.rs` — implémentation d'`AdminPlugin` (page, get_data, set_data)

**Files:**
- Create: `crates/ritornello-plugin-generic-input/src/admin.rs`
- Create: `crates/ritornello-plugin-generic-input/src/index.html`
- Modify: `crates/ritornello-plugin-generic-input/src/locales/en.toml`
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`

**Interfaces:**
- Consumes: `ritornello_plugin_sdk::AdminPlugin`, `crate::bindings::Bindings`, `crate::devices::Hub`, `crate::presets`, `ritornello_i18n::Catalog`.
- Produces:
  - `pub struct GenericInputAdmin { pub bindings_path: PathBuf, pub presets_root: PathBuf, pub input_root: PathBuf, pub hub: Hub, pub catalog: Arc<RwLock<Catalog>> }`
  - `impl AdminPlugin for GenericInputAdmin` : `fn page(&self) -> String`, `async fn get_data(&self) -> serde_json::Value`, `async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>`.
  - `pub const PAGE_KEYS: &[&str]` — clés i18n substituées dans la page.

- [ ] **Step 1: Gabarit HTML minimal et clé de titre**

Le gabarit complet arrive en Task 7 ; à ce stade la page est réduite mais fonctionnelle (elle sera remplacée intégralement).

Créer `crates/ritornello-plugin-generic-input/src/index.html` :

```html
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ritornello — {{admin_title}}</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 40rem; margin: 2rem auto; padding: 0 1rem; }
</style>
<h1>ritornello</h1>
<p id="msg">{{admin_title}}</p>
</html>
```

Ajouter la clé au début de `crates/ritornello-plugin-generic-input/src/locales/en.toml` :

```toml
# page d'admin
admin_title = "input bindings"
```

- [ ] **Step 2: Déclarer le module**

Dans `crates/ritornello-plugin-generic-input/src/main.rs` :

```rust
// Module câblé en Task 8 : pour l'instant utilisé uniquement par ses tests.
#[allow(dead_code)]
mod admin;
```

- [ ] **Step 3: Écrire `admin.rs` avec ses tests**

Créer `crates/ritornello-plugin-generic-input/src/admin.rs` :

```rust
use crate::bindings::Bindings;
use crate::devices::Hub;
use crate::presets;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::AdminPlugin;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Clés i18n substituées dans `index.html`. Un test vérifie qu'elles existent
/// toutes dans l'anglais embarqué, et qu'aucun jeton ne survit au rendu.
pub const PAGE_KEYS: &[&str] = &["admin_title"];

/// Opérations portées par `SetData`, discriminées par le champ `op`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Save { bindings: Bindings },
    Learn { device: String },
    CancelLearn,
    LoadPreset { device: String, preset: String },
    Rescan,
}

pub struct GenericInputAdmin {
    pub bindings_path: PathBuf,
    pub presets_root: PathBuf,
    pub input_root: PathBuf,
    pub hub: Hub,
    pub catalog: Arc<RwLock<Catalog>>,
}

#[async_trait::async_trait]
impl AdminPlugin for GenericInputAdmin {
    fn page(&self) -> String {
        let cat = self.catalog.read().unwrap();
        let mut html = include_str!("index.html").to_string();
        for key in PAGE_KEYS {
            html = html.replace(&format!("{{{{{key}}}}}"), cat.get(key));
        }
        html
    }

    async fn get_data(&self) -> serde_json::Value {
        // Aucune garde de verrou ne traverse un `.await` (il n'y en a aucun).
        let devices = self.hub.device_names();
        let bindings = self.hub.bindings.read().unwrap().clone();
        let learning = self.hub.learn.read().unwrap().snapshot();
        let presets = presets::list(&self.presets_root);
        serde_json::json!({
            "devices": devices,
            "bindings": bindings,
            "presets": presets,
            "learning": learning,
        })
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
            Op::Save { bindings } => {
                bindings.validate().map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                bindings.save(&self.bindings_path).map_err(|e| e.to_string())?;
                *self.hub.bindings.write().unwrap() = bindings;
                Ok(())
            }
            Op::Learn { device } => {
                self.hub.learn.write().unwrap().learn(&device);
                Ok(())
            }
            Op::CancelLearn => {
                self.hub.learn.write().unwrap().cancel();
                Ok(())
            }
            Op::LoadPreset { device, preset } => {
                // Rien n'est persisté : l'utilisateur enregistre ensuite.
                let bindings = presets::load(&self.presets_root, &preset)
                    .map_err(|e| e.message(&self.catalog.read().unwrap()))?;
                self.hub.bindings.write().unwrap().replace_device(&device, bindings);
                Ok(())
            }
            Op::Rescan => {
                let n = self.hub.open_new_devices(&self.input_root);
                tracing::info!("rescan: {n} nouveau(x) peripherique(s) ouvert(s)");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Binding, Device};
    use ritornello_proto::Command;
    use tokio::sync::mpsc;

    struct Fixture {
        admin: GenericInputAdmin,
        _rx: mpsc::Receiver<Command>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let presets_root = dir.path().join("presets");
        std::fs::create_dir_all(&presets_root).unwrap();
        std::fs::write(
            presets_root.join("mce.toml"),
            "[[bindings]]\ncode = 115\ncmd = \"VolumeUp\"\n",
        )
        .unwrap();
        let input_root = dir.path().join("input");
        std::fs::create_dir_all(&input_root).unwrap();

        let bindings = Bindings {
            devices: vec![Device {
                name: "eHome".into(),
                bindings: vec![Binding::new(2, &Command::Select(1))],
            }],
        };
        let (tx, rx) = mpsc::channel(8);
        let hub = Hub::new(bindings, tx);
        hub.open
            .write()
            .unwrap()
            .insert(std::path::PathBuf::from("/dev/input/event0"), "eHome".into());
        let catalog = Arc::new(RwLock::new(Catalog::load(
            "generic-input",
            "en",
            std::path::Path::new("/nonexistent"),
            crate::GENERIC_INPUT_EN,
        )));
        Fixture {
            admin: GenericInputAdmin {
                bindings_path: dir.path().join("input-bindings.toml"),
                presets_root,
                input_root,
                hub,
                catalog,
            },
            _rx: rx,
            _dir: dir,
        }
    }

    #[test]
    fn page_substitue_tous_les_jetons() {
        let f = fixture();
        let html = f.admin.page();
        assert!(html.contains("input bindings"));
        assert!(!html.contains("{{"), "jeton non substitue dans la page");
    }

    #[test]
    fn page_utilise_le_catalogue_de_la_langue_courante() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("generic-input")).unwrap();
        std::fs::write(
            dir.path().join("generic-input/fr.toml"),
            "admin_title = \"touches\"\n",
        )
        .unwrap();
        let mut f = fixture();
        f.admin.catalog = Arc::new(RwLock::new(Catalog::load(
            "generic-input",
            "fr",
            dir.path(),
            crate::GENERIC_INPUT_EN,
        )));
        assert!(f.admin.page().contains("touches"));
    }

    #[test]
    fn toutes_les_cles_de_page_existent_dans_len_embarque() {
        let en = ritornello_i18n::try_parse(crate::GENERIC_INPUT_EN).unwrap();
        for key in PAGE_KEYS {
            assert!(en.contains_key(*key), "cle absente de en.toml: {key}");
        }
    }

    #[tokio::test]
    async fn get_data_renvoie_devices_bindings_presets_learning() {
        let f = fixture();
        let v = f.admin.get_data().await;
        assert_eq!(v["devices"], serde_json::json!(["eHome"]));
        assert_eq!(v["bindings"]["devices"][0]["name"], "eHome");
        assert_eq!(v["bindings"]["devices"][0]["bindings"][0]["cmd"], "Select");
        assert_eq!(v["bindings"]["devices"][0]["bindings"][0]["arg"], 1);
        assert_eq!(v["presets"], serde_json::json!(["mce"]));
        assert_eq!(v["learning"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn save_valide_persiste_et_remplace_la_table() {
        let mut f = fixture();
        let op = serde_json::json!({
            "op": "save",
            "bindings": { "devices": [
                { "name": "USB Keyboard", "bindings": [{ "code": 57, "cmd": "PlayPause" }] }
            ]}
        });
        assert!(f.admin.set_data(op).await.is_ok());
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("USB Keyboard", 57),
            Some(Command::PlayPause)
        );
        assert_eq!(
            Bindings::load(&f.admin.bindings_path).resolve("USB Keyboard", 57),
            Some(Command::PlayPause)
        );
    }

    #[tokio::test]
    async fn save_invalide_renvoie_une_erreur_traduite_et_ne_persiste_pas() {
        let mut f = fixture();
        let op = serde_json::json!({
            "op": "save",
            "bindings": { "devices": [
                { "name": "X", "bindings": [
                    { "code": 1, "cmd": "Select", "arg": 1 },
                    { "code": 1, "cmd": "Mute" }
                ]}
            ]}
        });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("code 1"), "message inattendu: {err}");
        assert!(!f.admin.bindings_path.exists());
        // la table partagée est intacte
        assert_eq!(
            f.admin.hub.bindings.read().unwrap().resolve("eHome", 2),
            Some(Command::Select(1))
        );
    }

    #[tokio::test]
    async fn learn_puis_cancel_learn() {
        let mut f = fixture();
        assert!(f
            .admin
            .set_data(serde_json::json!({ "op": "learn", "device": "eHome" }))
            .await
            .is_ok());
        assert_eq!(f.admin.get_data().await["learning"]["device"], "eHome");
        assert_eq!(
            f.admin.get_data().await["learning"]["captured"],
            serde_json::Value::Null
        );
        assert!(f.admin.set_data(serde_json::json!({ "op": "cancel_learn" })).await.is_ok());
        assert_eq!(f.admin.get_data().await["learning"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn load_preset_remplace_en_memoire_sans_persister() {
        let mut f = fixture();
        let op = serde_json::json!({ "op": "load_preset", "device": "eHome", "preset": "mce" });
        assert!(f.admin.set_data(op).await.is_ok());
        let b = f.admin.hub.bindings.read().unwrap();
        assert_eq!(b.resolve("eHome", 115), Some(Command::VolumeUp));
        // les anciens bindings du périphérique ont été remplacés
        assert_eq!(b.resolve("eHome", 2), None);
        drop(b);
        // rien sur le disque
        assert!(!f.admin.bindings_path.exists());
    }

    #[tokio::test]
    async fn load_preset_inconnu_renvoie_une_erreur() {
        let mut f = fixture();
        let op = serde_json::json!({ "op": "load_preset", "device": "eHome", "preset": "zzz" });
        let err = f.admin.set_data(op).await.unwrap_err();
        assert!(err.contains("zzz"), "message inattendu: {err}");
    }

    #[tokio::test]
    async fn rescan_sans_peripherique_reussit() {
        let mut f = fixture();
        assert!(f.admin.set_data(serde_json::json!({ "op": "rescan" })).await.is_ok());
    }

    #[tokio::test]
    async fn op_inconnue_renvoie_une_erreur() {
        let mut f = fixture();
        let err = f.admin.set_data(serde_json::json!({ "op": "detruire" })).await.unwrap_err();
        assert!(err.starts_with("invalid request:"), "message inattendu: {err}");
        let err2 = f.admin.set_data(serde_json::json!({ "rien": 1 })).await.unwrap_err();
        assert!(err2.starts_with("invalid request:"), "message inattendu: {err2}");
    }
}
```

- [ ] **Step 4: Lancer les tests et clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : les 11 tests d'`admin` passent (plus les précédents) ; aucun warning clippy.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-plugin-generic-input
git commit -m "feat(generic-input): moitie admin (get_data complet, operations save/learn/cancel_learn/load_preset/rescan)"
```

---

### Task 7: Page d'admin complète et i18n (anglais embarqué + pack français)

**Files:**
- Modify: `crates/ritornello-plugin-generic-input/src/index.html`
- Modify: `crates/ritornello-plugin-generic-input/src/locales/en.toml`
- Create: `deploy/locales/generic-input/fr.toml`
- Modify: `crates/ritornello-plugin-generic-input/src/admin.rs`
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`

**Interfaces:**
- Consumes: routes du cœur `GET ./api/data` (JSON de `get_data`) et `PUT ./api/data` (`204` si accepté, `422` + `{"error": …}` sinon).
- Produces: `PAGE_KEYS` (38 clés), `en.toml` (43 clés), `deploy/locales/generic-input/fr.toml` (mêmes 43 clés).

- [ ] **Step 1: Tests d'abord — parité des packs et complétude des jetons**

Ajouter au module `tests` de `crates/ritornello-plugin-generic-input/src/admin.rs` :

```rust
    /// Pack français livré dans le dépôt.
    fn pack_fr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/locales/generic-input/fr.toml");
        std::fs::read_to_string(p).expect("pack fr livre")
    }

    #[test]
    fn parite_des_cles_entre_len_embarque_et_le_pack_fr() {
        let en = ritornello_i18n::try_parse(crate::GENERIC_INPUT_EN).unwrap();
        let fr = ritornello_i18n::try_parse(&pack_fr()).unwrap();
        let mut cles_en: Vec<&String> = en.keys().collect();
        let mut cles_fr: Vec<&String> = fr.keys().collect();
        cles_en.sort();
        cles_fr.sort();
        assert_eq!(cles_en, cles_fr, "jeux de cles en/fr divergents");
    }

    #[test]
    fn la_page_expose_les_21_actions() {
        let f = fixture();
        let html = f.admin.page();
        for label in [
            "Preset 1", "Preset 9", "Next preset", "Previous preset", "Volume +", "Volume -",
            "Mute", "Play/pause", "Stop", "Next track", "Previous track", "Eject",
            "Change source", "Standby",
        ] {
            assert!(html.contains(label), "libelle absent de la page: {label}");
        }
    }
```

Ajouter au module `tests` de `crates/ritornello-plugin-generic-input/src/main.rs` (créer le module s'il n'existe pas) :

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn en_embarque_generic_input_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(crate::GENERIC_INPUT_EN).unwrap().is_empty());
    }
}
```

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input"`
Expected : FAIL — `parite_des_cles_entre_len_embarque_et_le_pack_fr` panique (pack fr absent) et `la_page_expose_les_21_actions` échoue (page minimale).

- [ ] **Step 2: Anglais embarqué complet**

`crates/ritornello-plugin-generic-input/src/locales/en.toml` — contenu complet :

```toml
# messages de validation
duplicate_code = "code {code} is bound twice on {device}"
select_out_of_range = "preset {n} out of range 1-9 on {device}"
unknown_command = "unknown command bound to code {code} on {device}"
unknown_preset = "unknown preset: {preset}"
bad_request = "invalid request: {detail}"

# page d'admin
admin_title = "input bindings"
device_label = "Device"
btn_refresh = "Refresh"
col_action = "Action"
col_code = "Key code(s)"
btn_learn = "Learn"
btn_clear = "Clear"
preset_label = "Preset"
btn_load_preset = "Load"
btn_save = "Save"
btn_cancel = "Cancel"
learning_msg = "Press a key on the device…"
learn_timeout = "No key detected — cancelled"
saved = "Saved ✓"
save_error = "Error: "
load_error = "Loading error: "
no_device = "No input device detected"

# les 21 actions
act_select_1 = "Preset 1"
act_select_2 = "Preset 2"
act_select_3 = "Preset 3"
act_select_4 = "Preset 4"
act_select_5 = "Preset 5"
act_select_6 = "Preset 6"
act_select_7 = "Preset 7"
act_select_8 = "Preset 8"
act_select_9 = "Preset 9"
act_next = "Next preset"
act_prev = "Previous preset"
act_volume_up = "Volume +"
act_volume_down = "Volume -"
act_mute = "Mute"
act_play_pause = "Play/pause"
act_stop = "Stop"
act_next_track = "Next track"
act_prev_track = "Previous track"
act_eject = "Eject"
act_source_cycle = "Change source"
act_power = "Standby"
```

- [ ] **Step 3: Pack français**

Créer `deploy/locales/generic-input/fr.toml` — **mêmes 43 clés**, dans le même ordre :

```toml
duplicate_code = "code {code} lié deux fois sur {device}"
select_out_of_range = "présélection {n} hors bornes 1-9 sur {device}"
unknown_command = "commande inconnue liée au code {code} sur {device}"
unknown_preset = "preset inconnu : {preset}"
bad_request = "requête invalide : {detail}"

admin_title = "touches"
device_label = "Périphérique"
btn_refresh = "Rafraîchir"
col_action = "Action"
col_code = "Code(s) de touche"
btn_learn = "Apprendre"
btn_clear = "Effacer"
preset_label = "Preset"
btn_load_preset = "Charger"
btn_save = "Enregistrer"
btn_cancel = "Annuler"
learning_msg = "Appuyez sur une touche du périphérique…"
learn_timeout = "Aucune touche détectée — abandon"
saved = "Enregistré ✓"
save_error = "Erreur : "
load_error = "Erreur de chargement : "
no_device = "Aucun périphérique d'entrée détecté"

act_select_1 = "Présélection 1"
act_select_2 = "Présélection 2"
act_select_3 = "Présélection 3"
act_select_4 = "Présélection 4"
act_select_5 = "Présélection 5"
act_select_6 = "Présélection 6"
act_select_7 = "Présélection 7"
act_select_8 = "Présélection 8"
act_select_9 = "Présélection 9"
act_next = "Présélection suivante"
act_prev = "Présélection précédente"
act_volume_up = "Volume +"
act_volume_down = "Volume -"
act_mute = "Muet"
act_play_pause = "Lecture/pause"
act_stop = "Stop"
act_next_track = "Piste suivante"
act_prev_track = "Piste précédente"
act_eject = "Éjecter"
act_source_cycle = "Changement de source"
act_power = "Veille"
```

- [ ] **Step 4: Liste des clés substituées par `page()`**

Dans `crates/ritornello-plugin-generic-input/src/admin.rs`, remplacer `PAGE_KEYS` :

```rust
/// Clés i18n substituées dans `index.html`. Deux tests les gardent alignées :
/// toutes présentes dans l'anglais embarqué, et aucun jeton `{{…}}` survivant
/// au rendu.
pub const PAGE_KEYS: &[&str] = &[
    "admin_title",
    "device_label",
    "btn_refresh",
    "col_action",
    "col_code",
    "btn_learn",
    "btn_clear",
    "preset_label",
    "btn_load_preset",
    "btn_save",
    "btn_cancel",
    "learning_msg",
    "learn_timeout",
    "saved",
    "save_error",
    "load_error",
    "no_device",
    "act_select_1",
    "act_select_2",
    "act_select_3",
    "act_select_4",
    "act_select_5",
    "act_select_6",
    "act_select_7",
    "act_select_8",
    "act_select_9",
    "act_next",
    "act_prev",
    "act_volume_up",
    "act_volume_down",
    "act_mute",
    "act_play_pause",
    "act_stop",
    "act_next_track",
    "act_prev_track",
    "act_eject",
    "act_source_cycle",
    "act_power",
];
```

- [ ] **Step 5: Page complète**

`crates/ritornello-plugin-generic-input/src/index.html` — contenu complet (remplace le gabarit minimal) :

```html
<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ritornello — {{admin_title}}</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 44rem; margin: 2rem auto; padding: 0 1rem; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .25rem; text-align: left; }
  td.act { width: 12rem; }
  td.code { width: 9rem; }
  td.btn { width: 6rem; }
  input { width: 100%; box-sizing: border-box; padding: .4rem; }
  button { padding: .4rem .8rem; }
  .bar { margin: 1rem 0; display: flex; gap: .5rem; align-items: center; }
  #msg { margin-left: .5rem; }
</style>
<h1>ritornello</h1>

<div class="bar">
  <label for="dev">{{device_label}}</label>
  <select id="dev"></select>
  <button id="refresh">{{btn_refresh}}</button>
</div>

<table id="t">
  <thead><tr><th>{{col_action}}</th><th>{{col_code}}</th><th></th><th></th></tr></thead>
  <tbody></tbody>
</table>

<div class="bar">
  <label for="preset">{{preset_label}}</label>
  <select id="preset"></select>
  <button id="loadPreset">{{btn_load_preset}}</button>
</div>

<div class="bar">
  <button id="save">{{btn_save}}</button>
  <button id="cancel" hidden>{{btn_cancel}}</button>
  <span id="msg"></span>
</div>

<script>
// Les 21 actions : libellé traduit côté serveur, commande au format
// ritornello_proto::Command sérialisé (cmd/arg).
const ACTIONS = [
  { label: '{{act_select_1}}', cmd: { cmd: 'Select', arg: 1 } },
  { label: '{{act_select_2}}', cmd: { cmd: 'Select', arg: 2 } },
  { label: '{{act_select_3}}', cmd: { cmd: 'Select', arg: 3 } },
  { label: '{{act_select_4}}', cmd: { cmd: 'Select', arg: 4 } },
  { label: '{{act_select_5}}', cmd: { cmd: 'Select', arg: 5 } },
  { label: '{{act_select_6}}', cmd: { cmd: 'Select', arg: 6 } },
  { label: '{{act_select_7}}', cmd: { cmd: 'Select', arg: 7 } },
  { label: '{{act_select_8}}', cmd: { cmd: 'Select', arg: 8 } },
  { label: '{{act_select_9}}', cmd: { cmd: 'Select', arg: 9 } },
  { label: '{{act_next}}', cmd: { cmd: 'Next' } },
  { label: '{{act_prev}}', cmd: { cmd: 'Prev' } },
  { label: '{{act_volume_up}}', cmd: { cmd: 'VolumeUp' } },
  { label: '{{act_volume_down}}', cmd: { cmd: 'VolumeDown' } },
  { label: '{{act_mute}}', cmd: { cmd: 'Mute' } },
  { label: '{{act_play_pause}}', cmd: { cmd: 'PlayPause' } },
  { label: '{{act_stop}}', cmd: { cmd: 'Stop' } },
  { label: '{{act_next_track}}', cmd: { cmd: 'NextTrack' } },
  { label: '{{act_prev_track}}', cmd: { cmd: 'PrevTrack' } },
  { label: '{{act_eject}}', cmd: { cmd: 'Eject' } },
  { label: '{{act_source_cycle}}', cmd: { cmd: 'SourceCycle' } },
  { label: '{{act_power}}', cmd: { cmd: 'Power' } },
];
const T = {
  learn: '{{btn_learn}}',
  clear: '{{btn_clear}}',
  learning: '{{learning_msg}}',
  timeout: '{{learn_timeout}}',
  saved: '{{saved}}',
  saveError: '{{save_error}}',
  loadError: '{{load_error}}',
  noDevice: '{{no_device}}',
};

let state = { devices: [], bindings: { devices: [] }, presets: [], learning: null };
let sel = null;   // périphérique sélectionné
let timer = null; // sondage d'apprentissage

const $ = (id) => document.getElementById(id);
const msg = (t) => { $('msg').textContent = t; };
const sameCmd = (a, b) => a.cmd === b.cmd && (a.arg ?? null) === (b.arg ?? null);

async function fetchState() {
  const r = await fetch('./api/data');
  if (!r.ok) throw new Error('HTTP ' + r.status);
  return await r.json();
}

// Renvoie null si l'opération est acceptée (204), sinon le message d'erreur
// (corps JSON {"error": …} du 422).
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

function codesFor(device, action) {
  const d = state.bindings.devices.find((x) => x.name === device);
  if (!d) return '';
  return d.bindings.filter((b) => sameCmd(b, action.cmd)).map((b) => b.code).join(', ');
}

function option(value) {
  const o = document.createElement('option');
  o.value = value;
  o.textContent = value;
  return o;
}

function render() {
  const dev = $('dev');
  dev.innerHTML = '';
  state.devices.forEach((n) => dev.appendChild(option(n)));
  if (!state.devices.includes(sel)) sel = state.devices[0] || null;
  if (sel) dev.value = sel;

  const preset = $('preset');
  preset.innerHTML = '';
  state.presets.forEach((n) => preset.appendChild(option(n)));

  const tbody = $('t').querySelector('tbody');
  tbody.innerHTML = '';
  ACTIONS.forEach((a, i) => {
    const tr = document.createElement('tr');
    const tdA = document.createElement('td');
    tdA.className = 'act';
    tdA.textContent = a.label;
    const tdC = document.createElement('td');
    tdC.className = 'code';
    const input = document.createElement('input');
    input.id = 'c' + i;
    input.inputMode = 'numeric';
    input.value = sel ? codesFor(sel, a) : '';
    tdC.appendChild(input);
    const tdL = document.createElement('td');
    tdL.className = 'btn';
    const bl = document.createElement('button');
    bl.textContent = T.learn;
    bl.onclick = () => learn(i);
    tdL.appendChild(bl);
    const tdX = document.createElement('td');
    tdX.className = 'btn';
    const bx = document.createElement('button');
    bx.textContent = T.clear;
    bx.onclick = () => { input.value = ''; };
    tdX.appendChild(bx);
    tr.append(tdA, tdC, tdL, tdX);
    tbody.appendChild(tr);
  });
  if (!sel) msg(T.noDevice);
}

// Reconstruit la table complète : les autres périphériques sont préservés
// tels quels, seul le périphérique courant est réécrit depuis le tableau.
function collect() {
  const devices = state.bindings.devices.filter((d) => d.name !== sel);
  const bindings = [];
  ACTIONS.forEach((a, i) => {
    const raw = $('c' + i).value.trim();
    if (!raw) return;
    raw.split(',').forEach((part) => {
      const code = parseInt(part.trim(), 10);
      if (!Number.isNaN(code)) bindings.push(Object.assign({ code: code }, a.cmd));
    });
  });
  if (sel) devices.push({ name: sel, bindings: bindings });
  return { devices: devices };
}

async function save() {
  if (!sel) { msg(T.noDevice); return; }
  const table = collect();
  const err = await put({ op: 'save', bindings: table });
  if (err) { msg(T.saveError + err); return; }
  state.bindings = table;
  msg(T.saved);
}

async function stopLearn(text) {
  if (timer) { clearInterval(timer); timer = null; }
  $('cancel').hidden = true;
  await put({ op: 'cancel_learn' });
  msg(text);
}

async function learn(i) {
  if (!sel) { msg(T.noDevice); return; }
  if (timer) await stopLearn('');
  const err = await put({ op: 'learn', device: sel });
  if (err) { msg(err); return; }
  msg(T.learning);
  $('cancel').hidden = false;
  const deadline = Date.now() + 10000;
  timer = setInterval(async () => {
    let s;
    try { s = await fetchState(); } catch (e) { return; }
    const c = s.learning ? s.learning.captured : null;
    if (c !== null && c !== undefined) {
      $('c' + i).value = String(c);
      await stopLearn('');
      return;
    }
    if (Date.now() > deadline) await stopLearn(T.timeout);
  }, 300);
}

async function reload() {
  try {
    state = await fetchState();
    msg('');
    render();
  } catch (e) {
    msg(T.loadError + e.message);
  }
}

$('dev').onchange = () => { sel = $('dev').value; render(); };
$('refresh').onclick = async () => { await put({ op: 'rescan' }); await reload(); };
$('loadPreset').onclick = async () => {
  if (!sel) { msg(T.noDevice); return; }
  const err = await put({ op: 'load_preset', device: sel, preset: $('preset').value });
  if (err) { msg(err); return; }
  await reload();
};
$('save').onclick = save;
$('cancel').onclick = () => stopLearn('');
reload();
</script>
</html>
```

Note de conception : une ligne accepte **plusieurs codes séparés par des virgules** (la télécommande MCE lie par exemple 2 *et* 513 à `Select(1)`) ; « Apprendre » remplace le contenu de la case par le code capturé, « Effacer » la vide. Rien n'est envoyé au serveur avant « Enregistrer ».

- [ ] **Step 6: Lancer les tests et clippy**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : tous les tests passent, dont `page_substitue_tous_les_jetons` (aucun `{{` résiduel), `toutes_les_cles_de_page_existent_dans_len_embarque`, `parite_des_cles_entre_len_embarque_et_le_pack_fr`, `la_page_expose_les_21_actions` et `en_embarque_generic_input_est_non_vide` ; aucun warning clippy.

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-plugin-generic-input deploy/locales/generic-input
git commit -m "feat(generic-input): page d'admin complete (21 actions, apprentissage, presets) et packs en/fr"
```

---

### Task 8: Câblage de `main.rs`, déploiement et documentation

**Files:**
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs`
- Delete: `crates/ritornello-plugin-generic-input/src/input.rs`
- Delete: `crates/ritornello-plugin-generic-input/src/keymap.rs`
- Create: `deploy/input-bindings.example.toml`
- Modify: `deploy/deploy.sh`
- Modify: `README.md`

**Interfaces:**
- Consumes: `ritornello_plugin_sdk::{InputPlugin, run_input_plugin, run_admin_plugin}`, `crate::admin::GenericInputAdmin`, `crate::devices::{Hub, INPUT_DIR}`, `crate::bindings::Bindings`, `ritornello_i18n::Catalog`.
- Produces: binaire à deux moitiés indépendantes ; variables d'environnement `RITORNELLO_INPUT_BINDINGS` (défaut `/etc/ritornello/input-bindings.toml`), `RITORNELLO_INPUT_PRESETS` (défaut `/etc/ritornello/input-presets`), `RITORNELLO_LOCALES` (défaut `/etc/ritornello/locales`), `RITORNELLO_LOCALE` (défaut `en`). Disparaissent : `RITORNELLO_MCE_INPUT_NAME`, `RITORNELLO_MCE_DEVICE`.

- [ ] **Step 1: Test d'abord — les valeurs par défaut des chemins**

Remplacer le module `tests` de `crates/ritornello-plugin-generic-input/src/main.rs` par :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_embarque_generic_input_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(GENERIC_INPUT_EN).unwrap().is_empty());
    }

    #[test]
    fn chemins_par_defaut() {
        std::env::remove_var("RITORNELLO_INPUT_BINDINGS_TEST");
        assert_eq!(
            env_or("RITORNELLO_INPUT_BINDINGS_TEST", "/etc/ritornello/input-bindings.toml"),
            "/etc/ritornello/input-bindings.toml"
        );
        std::env::set_var("RITORNELLO_INPUT_BINDINGS_TEST", "/tmp/x.toml");
        assert_eq!(env_or("RITORNELLO_INPUT_BINDINGS_TEST", "/etc/ritornello/input-bindings.toml"), "/tmp/x.toml");
        std::env::remove_var("RITORNELLO_INPUT_BINDINGS_TEST");
    }
}
```

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input"`
Expected : PASS (ces tests passent déjà ; ils fixent le contrat avant la réécriture).

- [ ] **Step 2: Réécrire `main.rs`**

`crates/ritornello-plugin-generic-input/src/main.rs` — contenu complet (les `#[allow(dead_code)]` disparaissent, tout est câblé) :

```rust
mod admin;
mod bindings;
mod devices;
mod learn;
mod presets;

use crate::admin::GenericInputAdmin;
use crate::bindings::Bindings;
use crate::devices::Hub;
use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_sdk::{run_admin_plugin, run_input_plugin, InputPlugin};
use ritornello_proto::Command;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

pub(crate) const GENERIC_INPUT_EN: &str = include_str!("locales/en.toml");

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn arg_value(flag: &str) -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == flag).map(|i| PathBuf::from(&args[i + 1]))
}

/// Moitié Input : consomme le mpsc alimenté par toutes les tâches de lecture
/// evdev, quel que soit le périphérique d'origine.
struct EvdevInput {
    rx: mpsc::Receiver<Command>,
}

#[async_trait::async_trait]
impl InputPlugin for EvdevInput {
    async fn next_command(&mut self) -> Result<Command> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("toutes les boucles evdev sont terminees"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = arg_value("--socket").expect("--socket <path> requis");
    let admin_socket = arg_value("--admin-socket").expect("--admin-socket <path> requis");
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
    tracing::info!("{ouverts} peripherique(s) d'entree ouvert(s)");

    let admin = GenericInputAdmin {
        bindings_path,
        presets_root,
        input_root,
        hub,
        catalog,
    };

    // Les deux moitiés sont indépendantes : une panne de la socket admin ne
    // doit pas couper la télécommande, et réciproquement. Chaque moitié tourne
    // dans sa propre tâche tokio::spawn : une panique y est capturée dans le
    // JoinHandle (JoinError) au lieu de dérouler la pile de l'autre moitié.
    let input_handle = tokio::spawn(async move { run_input_plugin(EvdevInput { rx }, &socket_path).await });
    let admin_handle = tokio::spawn(async move { run_admin_plugin(admin, &admin_socket).await });

    let (input_res, admin_res) = tokio::join!(input_handle, admin_handle);

    match input_res {
        Ok(Ok(())) => tracing::warn!("plugin generic-input (moitie input) termine normalement"),
        Ok(Err(e)) => tracing::warn!("plugin generic-input (moitie input) erreur: {e}"),
        Err(join_err) => tracing::error!("plugin generic-input (moitie input) a panique: {join_err}"),
    }
    match admin_res {
        Ok(Ok(())) => tracing::warn!("plugin generic-input (moitie admin) termine normalement"),
        Ok(Err(e)) => tracing::warn!("plugin generic-input (moitie admin) erreur: {e}"),
        Err(join_err) => tracing::error!("plugin generic-input (moitie admin) a panique: {join_err}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_embarque_generic_input_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(GENERIC_INPUT_EN).unwrap().is_empty());
    }

    #[test]
    fn chemins_par_defaut() {
        std::env::remove_var("RITORNELLO_INPUT_BINDINGS_TEST");
        assert_eq!(
            env_or("RITORNELLO_INPUT_BINDINGS_TEST", "/etc/ritornello/input-bindings.toml"),
            "/etc/ritornello/input-bindings.toml"
        );
        std::env::set_var("RITORNELLO_INPUT_BINDINGS_TEST", "/tmp/x.toml");
        assert_eq!(
            env_or("RITORNELLO_INPUT_BINDINGS_TEST", "/etc/ritornello/input-bindings.toml"),
            "/tmp/x.toml"
        );
        std::env::remove_var("RITORNELLO_INPUT_BINDINGS_TEST");
    }
}
```

- [ ] **Step 3: Supprimer l'ancien code**

```bash
git rm crates/ritornello-plugin-generic-input/src/input.rs crates/ritornello-plugin-generic-input/src/keymap.rs
```

(La table de `keymap.rs` vit désormais dans `deploy/input-presets/mce.toml`, et la sélection d'un périphérique unique de `input.rs` n'a plus d'objet : tous les nœuds sont ouverts.)

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test -p ritornello-plugin-generic-input && cargo clippy -p ritornello-plugin-generic-input -- -D warnings"`
Expected : compile sans `#[allow(dead_code)]`, tous les tests passent, aucun warning clippy.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && grep -rn 'RITORNELLO_MCE' --exclude-dir=target --exclude-dir=.git . || echo CLEAN"`
Expected : `CLEAN`.

- [ ] **Step 4: Exemple de fichier de bindings**

Créer `deploy/input-bindings.example.toml` :

```toml
# Exemple de table de bindings : le preset MCE pre-lie a un recepteur
# infrarouge eHome. Adapter `name` au nom exact du peripherique tel qu'il
# apparait dans la liste deroulante de http://<hote>:8080/plugins/generic-input/
# (ou via `sudo evtest`). Tous les noeuds portant ce nom sont ecoutes.

[[devices]]
name = "eHome Infrared Transceiver"

[[devices.bindings]]
code = 2
cmd = "Select"
arg = 1

[[devices.bindings]]
code = 3
cmd = "Select"
arg = 2

[[devices.bindings]]
code = 4
cmd = "Select"
arg = 3

[[devices.bindings]]
code = 5
cmd = "Select"
arg = 4

[[devices.bindings]]
code = 6
cmd = "Select"
arg = 5

[[devices.bindings]]
code = 7
cmd = "Select"
arg = 6

[[devices.bindings]]
code = 8
cmd = "Select"
arg = 7

[[devices.bindings]]
code = 9
cmd = "Select"
arg = 8

[[devices.bindings]]
code = 10
cmd = "Select"
arg = 9

[[devices.bindings]]
code = 115
cmd = "VolumeUp"

[[devices.bindings]]
code = 114
cmd = "VolumeDown"

[[devices.bindings]]
code = 113
cmd = "Mute"

[[devices.bindings]]
code = 402
cmd = "Next"

[[devices.bindings]]
code = 403
cmd = "Prev"

[[devices.bindings]]
code = 164
cmd = "PlayPause"

[[devices.bindings]]
code = 163
cmd = "NextTrack"

[[devices.bindings]]
code = 165
cmd = "PrevTrack"

[[devices.bindings]]
code = 166
cmd = "Stop"

[[devices.bindings]]
code = 161
cmd = "Eject"

[[devices.bindings]]
code = 226
cmd = "SourceCycle"

[[devices.bindings]]
code = 116
cmd = "Power"

[[devices.bindings]]
code = 356
cmd = "Power"
```

- [ ] **Step 5: `deploy.sh` — installer les presets**

Dans `deploy/deploy.sh`, après le bloc qui copie les locales (`ssh "$PI" 'sudo cp -r /tmp/locales/. …'`), ajouter :

```bash
ssh "$PI" 'sudo mkdir -p /etc/ritornello/input-presets'
scp -r deploy/input-presets "$PI:/tmp/input-presets"
ssh "$PI" 'sudo cp -r /tmp/input-presets/. /etc/ritornello/input-presets/ && rm -rf /tmp/input-presets'
```

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && bash -n deploy/deploy.sh && echo SYNTAX-OK"`
Expected : `SYNTAX-OK`.

- [ ] **Step 6: README**

Dans `README.md` :

1. En-tête (ligne 3) : remplacer « télécommande MCE » par « télécommande configurable (evdev) ».
2. Section `## Portabilité` : remplacer « la télécommande MCE passe par `evdev` » par « la télécommande passe par `evdev` ».
3. Section `## Installation` (bloc `sudo cp deploy/…`) : ajouter les deux lignes

```
    sudo cp -r deploy/input-presets /etc/ritornello/input-presets
    sudo cp deploy/input-bindings.example.toml /etc/ritornello/input-bindings.toml
```

4. Section `## Plugins` : ajouter la puce

```markdown
- `ritornello-plugin-generic-input` déclare `admin = true` : il ouvre **tous**
  les périphériques evdev lisibles (non exclusif : le clavier continue de
  fonctionner normalement) et traduit les touches en commandes selon
  `/etc/ritornello/input-bindings.toml`. Sa page
  `http://<hôte>:8080/plugins/generic-input/` liste les périphériques
  détectés, permet d'apprendre une touche par action, de charger un preset
  livré (`mce`, `keyboard`) et d'enregistrer. Variables :
  `RITORNELLO_INPUT_BINDINGS`, `RITORNELLO_INPUT_PRESETS`, `RITORNELLO_LOCALE`.
```

5. Section `## Télécommande` : remplacer le paragraphe existant (« Si une touche ne répond pas : `sudo evtest` … `keymap.rs` ») par

```markdown
Si une touche ne répond pas, ouvrir `http://<hôte>:8080/plugins/generic-input/`,
choisir le périphérique dans la liste (bouton « Rafraîchir » s'il vient d'être
branché), cliquer « Apprendre » sur la ligne de l'action, appuyer sur la touche,
puis « Enregistrer ». Aucun redémarrage n'est nécessaire : la table est relue à
chaque appui. Pour partir d'une base, charger le preset `mce` ou `keyboard`.
```

6. Section `## Développement`, recette de lancement local : le plugin `generic-input` peut être ajouté au `plugins.toml` de `/tmp/rp` avec `admin = true`, et les variables `RITORNELLO_INPUT_BINDINGS=/tmp/rp/input-bindings.toml RITORNELLO_INPUT_PRESETS=deploy/input-presets` ajoutées à la ligne d'environnement.

- [ ] **Step 7: Vérification finale — tests, clippy, cross-compilation**

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo test --workspace"`
Expected : tous les tests du workspace verts.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cargo clippy --workspace -- -D warnings"`
Expected : aucun warning.

Run : `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello && cross build --release --workspace --target armv7-unknown-linux-gnueabihf"`
Expected : `Finished release` (nécessite Docker ; sur ce poste, `docker` tourne sous WSL).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(generic-input): cablage des deux moities, exemple de bindings, deploiement des presets et documentation"
```

---

## Notes de validation manuelle (hors CI, à faire une fois sur la machine cible)

Après la Task 8, déployer et vérifier :
- `http://<hôte>:8080/status` liste `generic-input` avec un lien d'admin.
- `http://<hôte>:8080/plugins/generic-input/` affiche la liste des périphériques réellement détectés.
- Brancher un clavier USB, cliquer « Rafraîchir » : il apparaît **sans recharger la page** et devient immédiatement apprenable.
- « Apprendre » sur « Volume + » : pendant l'attente, la touche pressée **ne déclenche pas** de volume + ; le code s'inscrit dans la ligne ; une touche d'un *autre* périphérique continue d'agir normalement.
- « Annuler » et le délai de 10 s sortent bien de l'apprentissage.
- Charger le preset `mce` puis recharger la page **sans** enregistrer : les bindings du preset sont visibles côté serveur (état en mémoire) mais `/etc/ritornello/input-bindings.toml` est inchangé ; après « Enregistrer », le fichier reflète la table.
- Lier deux fois le même code sur un périphérique → `422` et message traduit dans la zone de message.
- Débrancher le périphérique en cours d'écoute : le journal montre la fin de sa tâche, les autres périphériques continuent de fonctionner.
- Redémarrer le service : les bindings enregistrés sont rechargés (`journalctl -u ritornello -f`).
