# Activer et désactiver un greffon à chaud — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Depuis la page de configuration, éteindre un greffon — son processus meurt, ses câblages sont retirés — et le rallumer, le choix étant écrit dans `/etc/ritornello/plugins.toml`.

**Architecture:** Tout le chemin d'*arrivée* existe déjà (socket d'enregistrement permanent, `cabler_a_chaud`). Ce chantier écrit ce qui manque en face : une clé `enabled` lue et réécrite sans perdre les commentaires du fichier, la mort volontaire d'un processus dont le `Child` est aujourd'hui hors d'atteinte, le décâblage genre par genre, et une route HTTP qui persiste avant d'agir.

**Tech Stack:** Rust 2021, tokio (`select!`, `process::Child`, `oneshot`), `toml_edit`, `libc` (SIGTERM), axum, `futures::stream::FuturesUnordered`. IHM : Vue 3 + TypeScript, `@ritornello/ui`, vitest.

**Spec:** `docs/superpowers/specs/2026-08-24-greffons-actif-inactif-design.md`

## Global Constraints

- **Tests via WSL uniquement** — cargo n'existe pas dans Git Bash. Toute commande de test Rust :
  `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test <args>"`
- **Tests web : `npx vitest run` depuis `web/app`**, jamais depuis la racine du worktree. Les jonctions `node_modules` du worktree doivent exister (`vue-router`, `@ritornello/ui` — surtout pas pour `vite`).
- **`-D warnings`** est en vigueur : tout import inutilisé ou code mort casse la compilation.
- **Journaux en anglais**, commentaires et documentation en français. Convention du dépôt (commit efeda48).
- **Aucun message de refus n'atteint l'écran sans passer par le catalogue i18n.** Toute clé ajoutée à `crates/ritornello-core/src/locales/en.toml` doit l'être aussi à `deploy/locales/core/fr.toml` : un test Rust de parité échoue sinon.
- **Aucun test ne suppose une exécution rapide.** Pas d'assertion du type « ce délai ne peut pas être écoulé » : c'est la classe de flake déjà identifiée sur ce dépôt (commits 530312f, 8931d96).
- **Absent vaut actif.** `enabled` manquant = greffon activé, à la lecture comme à l'écriture. Ne jamais écrire `enabled = true`.
- Chemin du manifeste : `RITORNELLO_PLUGINS`, défaut `/etc/ritornello/plugins.toml`.

---

### Task 1 : la clé `enabled` dans le manifeste

**Files:**
- Modify: `crates/ritornello-core/src/plugins.rs` (`PluginConfig`, ses tests)

**Interfaces:**
- Consomme : rien.
- Produit : `PluginConfig { name: String, exec: String, enabled: bool }`, où `enabled` vaut `true` quand la clé est absente du TOML.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans le `mod tests` de `crates/ritornello-core/src/plugins.rs` :

```rust
#[test]
fn enabled_absent_vaut_actif_et_false_est_lu() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(
        &path,
        "[[plugin]]\nname = \"radio\"\nexec = \"/bin/true\"\n\n\
         [[plugin]]\nname = \"cd\"\nexec = \"/bin/true\"\nenabled = false\n",
    )
    .unwrap();
    let m = PluginManifest::load(&path).unwrap();
    // Un `plugins.toml` en service ne porte pas la clé : il doit continuer à
    // tout lancer.
    assert!(m.plugins[0].enabled, "sans mention, un greffon est actif");
    assert!(!m.plugins[1].enabled);
}
```

- [ ] **Step 2: Lancer le test pour le voir échouer**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core enabled_absent"`
Expected: FAIL — `no field 'enabled' on type 'PluginConfig'`.

- [ ] **Step 3: Ajouter le champ**

Dans `crates/ritornello-core/src/plugins.rs`, remplacer la structure :

```rust
/// Un greffon sans mention est **actif** : aucun `plugins.toml` en service ne
/// change de sens en gagnant cette clé, et « pas de clé = allumé » reste vrai
/// des deux côtés — `set_enabled` retire la clé au lieu d'écrire `true`.
fn actif_par_defaut() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginConfig {
    pub name: String,
    pub exec: String,
    /// Greffon lancé au démarrage et câblé, ou laissé éteint. Bascule depuis
    /// l'IHM d'admin (`PUT /api/plugins/:name/enabled`), persistée ici.
    #[serde(default = "actif_par_defaut")]
    pub enabled: bool,
}
```

Les deux littéraux `PluginConfig { name: ..., exec: ... }` du `mod tests` (autour de la ligne 260) gagnent `enabled: true`.

- [ ] **Step 4: Lancer les tests du module**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core plugins"`
Expected: PASS, tous les tests du module.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/plugins.rs
git commit -m "feat(core): le manifeste des greffons porte une cle enabled"
```

---

### Task 2 : réécrire `plugins.toml` sans perdre ses commentaires

`plugins.example.toml` est fait de commentaires — c'est là qu'est documenté à quoi sert chaque greffon — et `deploy.sh` y ajoute des blocs commentés sur un appareil en service. Un aller-retour `toml::to_string` les effacerait au premier basculement.

**Files:**
- Modify: `crates/ritornello-core/Cargo.toml` (dépendance `toml_edit`)
- Modify: `crates/ritornello-core/src/plugins.rs` (`set_enabled`, `ecrit_atomique`, tests)

**Interfaces:**
- Consomme : `PluginConfig.enabled` (Task 1).
- Produit : `pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> anyhow::Result<()>`. Erreur si le nom n'est pas déclaré dans le fichier, si le fichier est illisible, ou si l'écriture échoue.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans le `mod tests` de `crates/ritornello-core/src/plugins.rs` :

```rust
/// Un manifeste commenté comme celui du déploiement : c'est ce que la
/// réécriture doit rendre intact.
fn manifeste_commente() -> &'static str {
    "# Le tuner web.\n\
     [[plugin]]\n\
     name = \"radio\"\n\
     exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-radio\"\n\
     \n\
     # Les métadonnées : l'ordre de ce fichier arbitre.\n\
     [[plugin]]\n\
     name = \"musicbrainz\"\n\
     exec = \"/usr/local/lib/ritornello/plugins/ritornello-plugin-musicbrainz\"\n"
}

#[test]
fn desactiver_pose_la_cle_sans_toucher_aux_commentaires() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(&path, manifeste_commente()).unwrap();

    set_enabled(&path, "radio", false).unwrap();

    let apres = std::fs::read_to_string(&path).unwrap();
    assert!(apres.contains("# Le tuner web."), "commentaire de tête perdu");
    assert!(
        apres.contains("# Les métadonnées : l'ordre de ce fichier arbitre."),
        "commentaire du second bloc perdu"
    );
    let m = PluginManifest::load(&path).unwrap();
    assert!(!m.plugins[0].enabled);
    assert!(m.plugins[1].enabled, "le voisin n'a pas bougé");
    // L'ordre du fichier arbitre les `metadata` : le réécrire ne doit pas le
    // permuter.
    assert_eq!(m.plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["radio", "musicbrainz"]);
}

#[test]
fn reactiver_retire_la_cle_au_lieu_decrire_true() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(&path, manifeste_commente()).unwrap();

    set_enabled(&path, "radio", false).unwrap();
    set_enabled(&path, "radio", true).unwrap();

    let apres = std::fs::read_to_string(&path).unwrap();
    // « Pas de mention = allumé » doit rester vrai des deux côtés : un
    // fichier tout allumé ne porte aucune clé.
    assert!(!apres.contains("enabled"), "la clé aurait dû disparaître : {apres}");
    assert!(PluginManifest::load(&path).unwrap().plugins[0].enabled);
}

#[test]
fn un_nom_non_declare_est_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(&path, manifeste_commente()).unwrap();

    let avant = std::fs::read_to_string(&path).unwrap();
    assert!(set_enabled(&path, "inconnu", false).is_err());
    // Refus **sans effet de bord** : le fichier n'est pas réécrit.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), avant);
}

#[test]
fn aucun_fichier_temporaire_ne_survit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(&path, manifeste_commente()).unwrap();

    set_enabled(&path, "radio", false).unwrap();

    let restes: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(restes, ["plugins.toml"], "un fichier temporaire est resté");
}
```

- [ ] **Step 2: Lancer les tests pour les voir échouer**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core plugins::tests"`
Expected: FAIL — `cannot find function 'set_enabled'`.

- [ ] **Step 3: Ajouter la dépendance**

Dans `crates/ritornello-core/Cargo.toml`, à côté de `toml = "0.8"` :

```toml
# `toml` sait lire, pas réécrire sans tout aplatir : `plugins.toml` est fait
# de commentaires (à quoi sert chaque greffon), et `deploy.sh` y ajoute des
# blocs commentés. `toml_edit` préserve mise en forme et commentaires.
toml_edit = "0.22"
```

- [ ] **Step 4: Écrire l'implémentation**

Dans `crates/ritornello-core/src/plugins.rs`, après `impl PluginManifest` :

```rust
/// Bascule la clé `enabled` du greffon `name` dans le fichier, en place.
///
/// Désactiver pose `enabled = false` ; réactiver **retire la clé** plutôt que
/// d'écrire `true`, pour qu'un fichier tout allumé n'en porte aucune et que
/// « pas de mention = allumé » reste vrai des deux côtés.
///
/// Un nom non déclaré est une erreur et **ne réécrit rien** : c'est ce qui
/// permet à la couche HTTP de refuser avant d'agir.
pub fn set_enabled(path: &Path, name: &str, enabled: bool) -> Result<()> {
    let texte = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut =
        texte.parse().with_context(|| format!("parsing {}", path.display()))?;
    let blocs = doc
        .get_mut("plugin")
        .and_then(|item| item.as_array_of_tables_mut())
        .ok_or_else(|| anyhow::anyhow!("no [[plugin]] entry in {}", path.display()))?;
    let bloc = blocs
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
        .ok_or_else(|| anyhow::anyhow!("plugin '{name}' is not declared in {}", path.display()))?;
    if enabled {
        bloc.remove("enabled");
    } else {
        bloc["enabled"] = toml_edit::value(false);
    }
    ecrit_atomique(path, &doc.to_string())
}

/// Écrit par fichier temporaire voisin puis `rename` — atomique sur un même
/// système de fichiers, et l'idiome déjà employé pour les fichiers de
/// configuration écrits par le greffon `files`.
///
/// Un `plugins.toml` tronqué par une coupure de courant — un appareil qu'on
/// débranche — ne laisserait plus rien se lancer au démarrage suivant.
fn ecrit_atomique(path: &Path, contenu: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contenu).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming onto {}", path.display()))
}
```

- [ ] **Step 5: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core plugins"`
Expected: PASS (les quatre nouveaux et les anciens).

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-core/Cargo.toml Cargo.lock crates/ritornello-core/src/plugins.rs
git commit -m "feat(core): bascule de la cle enabled sans perdre les commentaires du manifeste"
```

---

### Task 3 : terminer un processus de greffon

**Files:**
- Modify: `crates/ritornello-core/src/plugins.rs` (`termine`, `GRACE_ARRET`, tests)

**Interfaces:**
- Consomme : rien.
- Produit : `pub const GRACE_ARRET: std::time::Duration` (2 s) et
  `pub async fn termine(child: &mut tokio::process::Child, grace: Duration) -> std::io::Result<std::process::ExitStatus>`.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans le `mod tests` de `crates/ritornello-core/src/plugins.rs` :

```rust
#[tokio::test]
async fn termine_arrete_un_processus_qui_dormait() {
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let statut = termine(&mut child, GRACE_ARRET).await.unwrap();
    // Terminé par signal : pas de code de sortie nul.
    assert!(!statut.success(), "le processus aurait dû être terminé : {statut:?}");
}

#[tokio::test]
async fn termine_insiste_quand_sigterm_est_ignore() {
    // Un greffon qui masque SIGTERM ne doit pas pouvoir retenir l'extinction.
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("trap '' TERM; sleep 30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Grâce courte : le test mesure la **retombée** sur SIGKILL, pas un délai.
    let statut = termine(&mut child, std::time::Duration::from_millis(200)).await.unwrap();
    assert!(!statut.success(), "SIGKILL aurait dû avoir raison de lui : {statut:?}");
}
```

- [ ] **Step 2: Lancer les tests pour les voir échouer**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core termine_"`
Expected: FAIL — `cannot find function 'termine'`.

- [ ] **Step 3: Écrire l'implémentation**

Dans `crates/ritornello-core/src/plugins.rs` (ajouter `use std::time::Duration;` en tête si absent) :

```rust
/// Temps laissé à un greffon entre `SIGTERM` et `SIGKILL`.
///
/// Deux secondes : aucun greffon n'a de nettoyage à faire aujourd'hui, et la
/// bascule vient d'une page web qui attend la réponse.
pub const GRACE_ARRET: Duration = Duration::from_secs(2);

/// Termine un greffon : `SIGTERM`, puis `SIGKILL` s'il s'attarde au-delà de
/// `grace`.
///
/// `SIGTERM` d'abord, comme pour mpv (`system.rs`) : c'est le signal qu'un
/// greffon pourra un jour intercepter pour rendre une console ou éteindre un
/// écran. Aucun ne le fait, et le défaut de Rust le termine aussitôt — mais
/// tuer d'entrée interdirait cette politesse pour toujours.
///
/// Rend le statut de sortie, jamais une attente sans fin : c'est tout l'objet
/// de la retombée sur `SIGKILL`, qu'aucun processus ne peut masquer.
pub async fn termine(
    child: &mut tokio::process::Child,
    grace: Duration,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = child.id() {
        // SAFETY : le `Child` est encore vivant ici, donc le processus n'a pas
        // été moissonné et son pid n'a pas pu être réattribué à un autre.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(statut) => statut,
        Err(_) => {
            tracing::warn!("plugin ignored SIGTERM, sending SIGKILL");
            child.kill().await?;
            child.wait().await
        }
    }
}
```

- [ ] **Step 4: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core plugins"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/plugins.rs
git commit -m "feat(core): terminer un greffon par SIGTERM puis SIGKILL"
```

---

### Task 4 : retirer une source du cœur

Aujourd'hui le cœur sait ajouter une source, jamais en retirer. `Command::SourceCycle` fait déjà tout le travail de bascule (arrêt, `Deactivate`, oublis, persistance, `Activate`) : on l'extrait pour ne pas en écrire une seconde version qui divergerait.

**Files:**
- Modify: `crates/ritornello-core/src/core.rs` (`bascule_source`, `remove_source`, `Command::SourceCycle`, tests)

**Interfaces:**
- Consomme : rien.
- Produit : `pub async fn remove_source(&mut self, name: &str) -> anyhow::Result<bool>` — `true` si la source était présente. Après l'appel, `active_source()` rend le nom de la suivante, ou `""` s'il n'en reste aucune.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans le `mod tests` de `crates/ritornello-core/src/core.rs`. **Le montage existe déjà** : `setup()` rend `(Core<FakePlayer>, journal du lecteur, journal des sources, récepteur d'état, TempDir)` avec `cd` et `radio` câblées, `source_order == ["cd", "radio"]` et **`radio` active** (c'est l'état persisté par défaut). Les appels journalisés ont la forme `"radio:Deactivate"` pour les sources, `"stop"` pour le lecteur. Ne pas inventer d'autre montage.

```rust
#[tokio::test]
async fn remove_source_bascule_sur_la_suivante() {
    let (mut core, _pc, source_calls, _rx, _d) = setup();
    assert_eq!(core.active_source(), "radio");

    assert!(core.remove_source("radio").await.unwrap());

    assert_eq!(core.active_source(), "cd", "la suivante du cycle prend la place");
    assert_eq!(core.source_order, vec!["cd".to_string()]);
    let calls = source_calls.lock().unwrap();
    assert!(
        calls.iter().any(|c| c == "radio:Deactivate"),
        "la sortante est prévenue avant de disparaître : {calls:?}"
    );
    assert!(calls.iter().any(|c| c == "cd:Activate"), "l'entrante est activée : {calls:?}");
}

#[tokio::test]
async fn remove_source_de_la_derniere_laisse_le_coeur_sans_source() {
    let (mut core, _pc, _sc, _rx, _d) = setup();
    assert!(core.remove_source("cd").await.unwrap());
    assert!(core.remove_source("radio").await.unwrap());

    // Aucune source est un état légitime : `demande_active` le tolère, et
    // démarrer sans source est accepté depuis l'enregistrement à chaud.
    assert_eq!(core.active_source(), "");
    assert!(core.source_order.is_empty());
    // Et une commande dans cet état ne panique pas.
    core.handle_input(InputMessage::from(Command::Next)).await.unwrap();
}

#[tokio::test]
async fn remove_source_dune_source_inactive_ne_touche_pas_a_ce_qui_joue() {
    let (mut core, player_calls, _sc, _rx, _d) = setup();

    assert!(core.remove_source("cd").await.unwrap());

    assert_eq!(core.active_source(), "radio");
    assert_eq!(core.source_order, vec!["radio".to_string()]);
    assert!(
        !player_calls.lock().unwrap().iter().any(|c| c == "stop"),
        "retirer une source inactive n'arrête pas ce qui joue"
    );
}

#[tokio::test]
async fn remove_source_dun_nom_inconnu_est_un_non_evenement() {
    let (mut core, _pc, _sc, _rx, _d) = setup();
    assert!(!core.remove_source("jamais-vu").await.unwrap());
    assert_eq!(core.active_source(), "radio");
    assert_eq!(core.source_order, vec!["cd".to_string(), "radio".into()]);
}
```

`InputMessage::from(Command::Next)` : reprendre la forme exacte employée par les tests voisins qui appellent `handle_input` — si elle diffère, suivre la leur.

- [ ] **Step 2: Lancer les tests pour les voir échouer**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core remove_source"`
Expected: FAIL — `no method named 'remove_source'`.

- [ ] **Step 3: Extraire la bascule de `SourceCycle`**

Dans `crates/ritornello-core/src/core.rs`, ajouter à côté de `add_source` :

```rust
/// Bascule vers `suivante` (ou vers **aucune** source si `None`) : arrêt,
/// `Deactivate` de la sortante, oublis, persistance, `Activate` de l'entrante.
///
/// Extraite de `Command::SourceCycle` et non recopiée : la désactivation d'un
/// greffon fait exactement la même chose, et deux versions de cette séquence
/// divergeraient au premier oubli ajouté d'un côté.
async fn bascule_source(&mut self, suivante: Option<String>) -> Result<()> {
    self.expecting_stream = false;
    self.lecture = false;
    self.player.stop().await?;
    if let Err(e) = self.demande_active(SourceReq::Deactivate).await {
        tracing::debug!("deactivate: {e}");
    }
    self.active_source = suivante.unwrap_or_default();
    self.set_identity(None);
    self.preset_count = None;
    self.source_status = None;
    self.can_eject = false;
    self.retry_count = 0;
    self.persist();
    if let Some(action) = self.demande_active(SourceReq::Activate).await? {
        self.apply(action).await?;
    }
    Ok(())
}
```

Puis remplacer le corps de `Command::SourceCycle` par (en **conservant** tous ses commentaires, déplacés dans `bascule_source` là où ils décrivent la séquence) :

```rust
Command::SourceCycle => {
    let idx =
        self.source_order.iter().position(|n| n == &self.active_source).unwrap_or(0);
    let next_idx = (idx + 1) % self.source_order.len().max(1);
    let suivante = self.source_order.get(next_idx).cloned();
    self.bascule_source(suivante).await?;
}
```

- [ ] **Step 4: Vérifier que l'extraction n'a rien changé**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core core::"`
Expected: PASS pour tous les tests existants de `SourceCycle` — c'est **eux** qui prouvent la fidélité de l'extraction. Les quatre nouveaux échouent encore.

- [ ] **Step 5: Écrire `remove_source`**

```rust
/// Retire une source décâblée — un greffon qu'on vient d'éteindre depuis
/// l'IHM. Rend `false` si ce nom n'était pas une source.
///
/// Si c'était l'active, la **suivante du cycle** prend sa place, ou aucune
/// s'il n'en reste pas : `demande_active` tolère déjà l'absence de source, et
/// démarrer sans source est légitime depuis l'enregistrement à chaud.
///
/// L'ordre est délicat : la bascule doit avoir lieu **avant** le retrait de la
/// table, parce que c'est elle qui envoie `Deactivate` à la source sortante —
/// retirée d'abord, elle ne recevrait rien et le greffon garderait son état
/// interne pour sa prochaine vie.
pub async fn remove_source(&mut self, name: &str) -> Result<bool> {
    let Some(pos) = self.source_order.iter().position(|n| n == name) else {
        return Ok(false);
    };
    if self.active_source == name {
        let suivante = if self.source_order.len() > 1 {
            Some(self.source_order[(pos + 1) % self.source_order.len()].clone())
        } else {
            None
        };
        self.bascule_source(suivante).await?;
    }
    self.sources.remove(name);
    self.source_order.remove(pos);
    Ok(true)
}
```

- [ ] **Step 6: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core"`
Expected: PASS partout.

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-core/src/core.rs
git commit -m "feat(core): retirer une source a chaud, bascule extraite du cycle de sources"
```

---

### Task 5 : l'état « désactivé » sur la page de statut

**Files:**
- Modify: `crates/ritornello-core/src/status.rs` (`PluginStatus`, `desactive`, tests)
- Modify: `crates/ritornello-core/src/register.rs` (`demarrage_refuse`, tests)

**Interfaces:**
- Consomme : rien.
- Produit : `PluginStatus { .., pub disabled: bool }` (omis du JSON quand faux),
  `PluginStatus::desactive(name: &str) -> PluginStatus`,
  `register::demarrage_refuse(actifs_declares: usize, lances: &[String], g: &Gathered) -> bool`.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans `crates/ritornello-core/src/status.rs`, `mod tests` :

```rust
#[test]
fn une_ligne_desactivee_ne_promet_rien() {
    let l = PluginStatus::desactive("cd");
    assert!(l.disabled);
    assert!(!l.connected, "aucun processus : rien n'est joint");
    assert!(!l.stalled, "il ne se tait pas, il n'existe pas");
    assert!(!l.admin, "pas de page d'admin à atteindre");
    assert_eq!(l.kind, "unknown");
}

#[test]
fn disabled_est_omis_quand_il_est_faux() {
    // Idiome de `stalled` : aucune trame existante ne change de forme.
    let json = serde_json::to_string(&PluginStatus::genre("radio", "source", true, false)).unwrap();
    assert!(!json.contains("disabled"), "{json}");
    let json = serde_json::to_string(&PluginStatus::desactive("cd")).unwrap();
    assert!(json.contains("\"disabled\":true"), "{json}");
}
```

Dans `crates/ritornello-core/src/register.rs`, `mod tests` :

```rust
#[test]
fn tout_eteindre_nest_pas_une_panne() {
    let g = Gathered::default();
    // Aucun greffon actif déclaré : rien n'a été lancé, et c'est voulu. Le
    // cœur doit démarrer — sans son IHM, plus personne ne pourrait rallumer.
    assert!(!demarrage_refuse(0, &[], &g));
    // Des greffons actifs déclarés, mais plus aucun processus vivant : c'est
    // l'erreur de configuration que le refus existe pour signaler.
    assert!(demarrage_refuse(2, &[], &g));
}

#[test]
fn un_seul_vivant_suffit_a_demarrer() {
    let mut g = Gathered::default();
    g.morts.push("cd".into());
    assert!(!demarrage_refuse(2, &["radio".into(), "cd".into()], &g));
}
```

Si `Gathered` ne dérive pas `Default`, l'ajouter (`#[derive(Debug, Default)]`) — les trois champs sont des collections.

- [ ] **Step 2: Lancer les tests pour les voir échouer**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core"`
(`cargo test` n'accepte **qu'un seul** filtre positionnel : pour viser plusieurs nouveaux tests d'un coup, lancer la suite du paquet.)
Expected: FAIL — fonctions et champ absents.

- [ ] **Step 3: Ajouter le champ et le constructeur**

Dans `crates/ritornello-core/src/status.rs`, dans `PluginStatus` (après `stalled`) :

```rust
    /// Greffon éteint depuis l'IHM : aucun processus, aucun câblage, et le
    /// manifeste porte `enabled = false`. La ligne reste affichée — sans elle,
    /// on ne pourrait plus le rallumer.
    ///
    /// Additif comme `stalled` : absent du JSON quand il est faux, donc aucune
    /// trame existante ne change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
```

Les constructeurs `genre` et `genre_inconnu` posent `disabled: false`. Ajouter :

```rust
    /// Ligne d'un greffon éteint. Ni genre ni page d'admin : il n'a rien
    /// annoncé et n'annoncera rien tant qu'il ne sera pas rallumé.
    pub fn desactive(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled: false,
            disabled: true,
        }
    }
```

- [ ] **Step 4: Ajouter le garde-fou du démarrage**

Dans `crates/ritornello-core/src/register.rs`, sous `un_greffon_vivant` :

```rust
/// Le démarrage doit-il être refusé ?
///
/// `un_greffon_vivant` ne suffit plus depuis qu'un greffon peut être éteint :
/// tout éteindre ne lance aucun processus, et le refus mettrait alors le cœur
/// en boucle de redémarrage systemd — **IHM comprise**, donc sans plus aucun
/// moyen de rallumer quoi que ce soit. Tout éteint est une configuration, pas
/// une panne.
///
/// Le refus ne reste que pour ce qu'il visait : des greffons déclarés actifs,
/// et plus un seul processus vivant pour s'annoncer.
pub fn demarrage_refuse(actifs_declares: usize, lances: &[String], g: &Gathered) -> bool {
    actifs_declares > 0 && !un_greffon_vivant(lances, g)
}
```

- [ ] **Step 5: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core"`
Expected: PASS. Les littéraux `PluginStatus { .. }` existants (dans `replace_plugin_lines` et les tests) compilent car ils passent par les constructeurs ou par `..PluginStatus::genre_inconnu(..)`.

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-core/src/status.rs crates/ritornello-core/src/register.rs
git commit -m "feat(core): etat desactive sur la page de statut, et tout eteindre n est pas une panne"
```

---

### Task 6 : le démarrage n'allume que les greffons actifs

**Files:**
- Modify: `crates/ritornello-core/src/main.rs` (boucle de lancement, supervision, appel du garde-fou)

**Interfaces:**
- Consomme : `PluginConfig.enabled` (Task 1), `PluginStatus::desactive` (Task 5), `register::demarrage_refuse` (Task 5), `plugins::termine` + `GRACE_ARRET` (Task 3).
- Produit : dans `main`, les variables `kill_triggers: HashMap<String, tokio::sync::oneshot::Sender<()>>`, `generations: HashMap<String, u64>`, et `plugin_waits: FuturesUnordered<SortieGreffon>` où
  `type SortieGreffon = futures::future::BoxFuture<'static, (String, u64, std::io::Result<std::process::ExitStatus>, bool)>` — nom, génération, statut, mort voulue.
  **Le type est boxé et nommé** : les deux endroits qui poussent une future de supervision (démarrage, rallumage de la Task 7) doivent produire *le même* type, ce qu'un `impl Future` rendu par deux fonctions différentes ne fait pas — chaque `impl Future` est un type opaque distinct, et le `FuturesUnordered` n'en accepte qu'un.

- [ ] **Step 1: Lancer les seuls greffons actifs**

Dans `crates/ritornello-core/src/main.rs`, remplacer la boucle `for p in &manifest.plugins` :

```rust
    let mut plugin_waits: FuturesUnordered<SortieGreffon> = FuturesUnordered::new();
    let mut lances: Vec<String> = Vec::new();
    let mut plugin_statuses = Vec::new();
    // Déclencheurs d'extinction, un par processus vivant : c'est la seule
    // prise sur un `Child` déplacé dans sa future de supervision.
    let mut kill_triggers: HashMap<String, tokio::sync::oneshot::Sender<()>> = HashMap::new();
    // Génération de lancement, par nom. Éteindre puis rallumer aussitôt fait
    // arriver la mort de l'**ancien** processus après le câblage du nouveau :
    // sans ce compteur, cette mort effacerait des lignes de statut qui
    // décrivent déjà le nouveau. Voir le bras `plugin_waits.next()`.
    let mut generations: HashMap<String, u64> = HashMap::new();

    for p in &manifest.plugins {
        generations.insert(p.name.clone(), 0);
        if !p.enabled {
            // Éteint : on ne lance rien, mais la ligne reste — sans elle, la
            // page ne le montrerait plus et il serait irrécupérable.
            tracing::info!("plugin {} is disabled, not launching it", p.name);
            plugin_statuses.push(PluginStatus::desactive(&p.name));
            continue;
        }
        let prefix = sockets_dir.join(&p.name);
        match plugins::spawn(&p.exec, &register_path, &p.name, &prefix, persisted.locale.as_deref())
        {
            Ok(child) => {
                let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
                kill_triggers.insert(p.name.clone(), kill_tx);
                plugin_waits.push(supervise(p.name.clone(), 0, child, kill_rx));
                lances.push(p.name.clone());
            }
            Err(e) => {
                tracing::warn!("failed to launch plugin {}: {e:#}", p.name);
                plugin_statuses.push(PluginStatus::genre_inconnu(&p.name, false));
            }
        }
    }
```

- [ ] **Step 2: Écrire la future de supervision**

Toujours dans `main.rs`, à côté de `relais_afficheur` :

```rust
/// Ce qu'une future de supervision rend : nom, génération, statut de sortie,
/// et si la mort avait été demandée.
///
/// Boxée, donc **nommée** : le démarrage et le rallumage poussent tous deux
/// dans le même `FuturesUnordered`, et deux fonctions rendant chacune un
/// `impl Future` rendent deux types opaques distincts, qu'aucune collection
/// n'accepte ensemble. Une allocation par lancement de greffon, huit au
/// démarrage.
type SortieGreffon =
    futures::future::BoxFuture<'static, (String, u64, std::io::Result<std::process::ExitStatus>, bool)>;

/// Surveille un greffon jusqu'à sa mort, qu'elle soit subie ou demandée.
///
/// Une fonction, et non un `async move` recopié aux deux endroits qui lancent
/// un greffon (démarrage et rallumage) : c'est le seul endroit qui sait que
/// `kill_rx` veut dire « termine-le ».
///
/// Le `select!` ne fait que **choisir** — aucun de ses bras ne touche à
/// `child` — pour que l'emprunt mutable des futures soit rendu avant le
/// `termine` qui suit. Rappeler `wait()` après coup est sans risque : tokio
/// mémorise le statut du processus déjà moissonné.
///
/// Rend `(nom, génération, statut, voulue)`. La génération est ce qui permet à
/// la boucle principale d'ignorer la mort d'une incarnation précédente,
/// arrivée après le rallumage de la suivante.
fn supervise(
    nom: String,
    generation: u64,
    child: tokio::process::Child,
    kill_rx: tokio::sync::oneshot::Receiver<()>,
) -> SortieGreffon {
    use futures::FutureExt;
    async move {
        let mut child = child;
        let voulue = tokio::select! {
            _ = kill_rx => true,
            _ = child.wait() => false,
        };
        let statut = if voulue {
            plugins::termine(&mut child, plugins::GRACE_ARRET).await
        } else {
            child.wait().await
        };
        (nom, generation, statut, voulue)
    }
    .boxed()
}
```

- [ ] **Step 3: Adapter les deux consommateurs du tuple**

`register::gather` reçoit `(&mut plugin_waits).map(|(nom, _statut)| nom)` : passer à `|(nom, _gen, _statut, _voulue)| nom`.

Le bras du `select!` principal devient :

```rust
            Some((name, gen, status, voulue)) = plugin_waits.next() => {
                // Mort d'une incarnation périmée : le greffon a été rallumé
                // entre-temps, et les lignes de statut décrivent déjà le
                // nouveau processus. Marquer « déconnecté » ici les
                // effacerait au profit d'une mort qui n'a plus cours.
                if generations.get(&name).copied() != Some(gen) {
                    tracing::debug!("plugin {name} generation {gen} exited after being replaced");
                } else if voulue {
                    tracing::info!("plugin {name} stopped: disabled from the admin UI");
                } else {
                    tracing::warn!("plugin {name} exited: {status:?}");
                    crate::status::mark_plugin_disconnected(&mut *status_state.write().await, &name);
                }
            }
```

- [ ] **Step 4: Appeler le nouveau garde-fou**

Remplacer le refus de démarrage :

```rust
    let actifs_declares = manifest.plugins.iter().filter(|p| p.enabled).count();
    if register::demarrage_refuse(actifs_declares, &lances, &rassemble) {
        anyhow::bail!(
            "no plugin process alive (every enabled plugin failed to launch or exited)"
        );
    }
    if actifs_declares == 0 {
        tracing::warn!(
            "every plugin is disabled in plugins.toml: starting anyway so they can be re-enabled from the admin UI"
        );
    }
```

- [ ] **Step 5: Ne pas câbler ce qui est éteint**

La boucle de câblage `for nom in &ordre_manifeste` s'appuie sur `rassemble.announcements` : un greffon jamais lancé n'y est pas, il est donc déjà ignoré. Ne rien changer, mais **vérifier** que `ordre_manifeste` reste construit depuis `manifest.plugins` en entier (l'ordre du fichier arbitre les `metadata`, éteints compris — un greffon éteint puis rallumé doit retrouver sa priorité).

- [ ] **Step 6: Compiler et lancer toute la suite**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core"`
Expected: PASS, et aucun avertissement (le dépôt est en `-D warnings`).

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-core/src/main.rs
git commit -m "feat(core): le demarrage n allume que les greffons actifs, supervision avec declencheur"
```

---

### Task 7 : éteindre et rallumer à chaud

**Files:**
- Modify: `crates/ritornello-core/src/main.rs` (`OrdreGreffon`, `eteindre_a_chaud`, `rallume`, bras du `select!`)

**Interfaces:**
- Consomme : Tasks 3 à 6.
- Produit : `status::OrdreGreffon { nom: String, actif: bool, ack: tokio::sync::oneshot::Sender<bool> }` — déclarée ici, dans `status.rs`, parce que la couche HTTP de la Task 8 l'émet et que `main.rs` n'est pas importable. Et, dans `main`, la table `execs: HashMap<String, String>` (nom → `exec`).

- [ ] **Step 1: Déclarer l'ordre**

Dans `crates/ritornello-core/src/status.rs`, à côté de `PluginStatus` :

```rust
/// Ordre d'allumage ou d'extinction, de la couche HTTP vers la boucle du cœur.
///
/// L'accusé est un `oneshot` et non un simple envoi : la page attend une
/// réponse qui décrive un état déjà vrai, sinon elle se rafraîchirait sur un
/// état intermédiaire. `bool` et non `Result` : la seule chose que le cœur
/// puisse rater est le lancement d'un binaire, dont la cause exacte part au
/// journal — que l'IHM montre déjà — pendant que l'écran reçoit un message du
/// catalogue.
pub struct OrdreGreffon {
    pub nom: String,
    pub actif: bool,
    pub ack: tokio::sync::oneshot::Sender<bool>,
}
```

- [ ] **Step 2: Retenir les `exec`**

Dans `main.rs`, à côté d'`ordre_manifeste` :

```rust
    // L'ordre du fichier arbitre les `metadata` ; l'`exec`, lui, ne servait
    // qu'au lancement initial. Rallumer un greffon le redemande.
    let execs: HashMap<String, String> =
        manifest.plugins.iter().map(|p| (p.name.clone(), p.exec.clone())).collect();
```

- [ ] **Step 3: Écrire l'extinction à chaud**

Dans `main.rs`, à côté de `cabler_a_chaud` :

```rust
/// Éteint un greffon : on demande sa mort, puis on retire **tout** ce que le
/// cœur tenait de lui.
///
/// Le décâblage est fait ici et non au retour de sa mort : la page attend une
/// réponse, et elle doit décrire un état déjà vrai. Le processus, lui, meurt à
/// son rythme — au pire deux secondes plus tard, `SIGKILL` en main — et sa
/// sortie ne fera plus que produire une ligne de journal.
///
/// Les afficheurs et les entrées n'ont rien d'explicite à retirer : leurs
/// relais sortent de boucle au premier échec d'envoi ou sur EOF, ce que la
/// mort du socket provoque.
async fn eteindre_a_chaud<P: player::Player>(
    nom: &str,
    fils: &FilsChaud,
    core: &mut core::Core<P>,
    rassemble: &mut register::Gathered,
    kill_triggers: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
) {
    tracing::info!("disabling plugin {nom}: killing it and unwiring everything it served");
    if let Some(tx) = kill_triggers.remove(nom) {
        // Le récepteur est dans la future de supervision : une erreur
        // d'envoi voudrait dire qu'elle est déjà finie, donc que le processus
        // est déjà mort. Rien à rattraper.
        let _ = tx.send(());
    }
    if let Err(e) = core.remove_source(nom).await {
        tracing::warn!("unwiring source {nom}: {e:#}");
    }
    // Le nom sort du rassemblement, puis l'ordre d'arbitrage est recalculé en
    // **entier** depuis le manifeste — le chemin qu'emprunte déjà toute
    // annonce tardive, et la seule façon qu'un greffon rallumé retrouve sa
    // priorité de fichier.
    rassemble.announcements.remove(nom);
    rassemble.figes.retain(|n| n != nom);
    rassemble.morts.retain(|n| n != nom);
    core.set_metadata_order(register::metadata_order(&fils.ordre_manifeste, rassemble));
    // Retiré, sinon `/plugins/<nom>/` attendrait les 5 s du protocole d'admin
    // — sériel, donc en retenant la page — pour finir en erreur, là où un 404
    // franc dit tout de suite qu'il n'y a rien à cette adresse.
    fils.admin_backends.write().await.remove(nom);
    let mut statuts = fils.status_state.write().await;
    status::replace_plugin_lines(&mut statuts, nom, vec![PluginStatus::desactive(nom)], false);
    statuts.active_source = core.active_source().to_string();
}
```

- [ ] **Step 4: Écrire le rallumage**

```rust
/// Rallume un greffon : on relance son binaire, et c'est tout.
///
/// Le câblage n'est **pas** fait ici : le greffon va s'annoncer sur le socket
/// d'enregistrement, que le cœur tient ouvert pour la vie du processus, et
/// `cabler_a_chaud` fera le reste. C'est le chemin d'un greffon relancé à la
/// main, déjà éprouvé.
///
/// D'ici là, la ligne dit « figé » : lancé, pas encore annoncé. C'est
/// exactement ce que le mot veut dire, et la page n'a pas besoin d'un
/// quatrième état pour une poignée de secondes.
///
/// Rend `false` si le binaire n'a pas pu être lancé — le chemin d'`exec` a
/// changé, le fichier n'est plus exécutable. La cause précise part au journal,
/// que l'IHM affiche déjà dans sa popin d'erreurs.
async fn rallume(
    nom: &str,
    exec: &str,
    generation: u64,
    fils: &FilsChaud,
    register_path: &Path,
    locale: Option<&str>,
    kill_triggers: &mut HashMap<String, tokio::sync::oneshot::Sender<()>>,
) -> Option<SortieGreffon> {
    let prefix = fils.sockets_dir.join(nom);
    match plugins::spawn(exec, register_path, nom, &prefix, locale) {
        Ok(child) => {
            tracing::info!("plugin {nom} re-enabled, launched again");
            let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
            kill_triggers.insert(nom.to_string(), kill_tx);
            let mut statuts = fils.status_state.write().await;
            status::replace_plugin_lines(
                &mut statuts,
                nom,
                vec![PluginStatus::genre_inconnu(nom, true)],
                false,
            );
            Some(supervise(nom.to_string(), generation, child, kill_rx))
        }
        Err(e) => {
            tracing::warn!("failed to launch plugin {nom}: {e:#}");
            None
        }
    }
}
```

- [ ] **Step 5: Brancher le bras du `select!`**

Déclarer le canal avec les autres, avant la boucle :

```rust
    let (greffon_tx, mut greffon_rx) = mpsc::channel::<status::OrdreGreffon>(4);
```

et ajouter le bras :

```rust
            Some(ordre) = greffon_rx.recv() => {
                let ok = if ordre.actif {
                    let generation = generations.entry(ordre.nom.clone()).or_insert(0);
                    *generation += 1;
                    let generation = *generation;
                    match execs.get(&ordre.nom) {
                        Some(exec) => {
                            match rallume(
                                &ordre.nom,
                                exec,
                                generation,
                                &fils_chaud,
                                &register_path,
                                core.locale_courante().as_deref(),
                                &mut kill_triggers,
                            )
                            .await
                            {
                                Some(fut) => {
                                    plugin_waits.push(fut);
                                    true
                                }
                                None => false,
                            }
                        }
                        // Nom refusé bien avant ici par la couche HTTP : c'est
                        // une garde, pas un cas d'usage.
                        None => false,
                    }
                } else {
                    eteindre_a_chaud(
                        &ordre.nom,
                        &fils_chaud,
                        &mut core,
                        &mut rassemble,
                        &mut kill_triggers,
                    )
                    .await;
                    true
                };
                // Le demandeur attend : un accusé perdu laisserait sa requête
                // HTTP pendre jusqu'au bout de son propre délai.
                let _ = ordre.ack.send(ok);
            }
```

`core.locale_courante()` : si `Core` n'expose pas la langue courante, ajouter l'accesseur `pub fn locale_courante(&self) -> Option<String> { self.locale.clone() }` dans `core.rs` — la langue est passée au lancement via `RITORNELLO_LOCALE`, et un greffon rallumé sur un appareil en français doit revenir en français (le piège déjà rencontré avec `cd` qui réaffichait `NO DISC`).

- [ ] **Step 6: Compiler**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo build -p ritornello-core"`
Expected: compile sans avertissement. `greffon_tx` n'est pas encore utilisé : le passer à `AppState` en Task 8 le consommera — si le compilateur s'en plaint d'ici là, enchaîner Task 8 sans commit intermédiaire.

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-core/src/main.rs crates/ritornello-core/src/core.rs
git commit -m "feat(core): eteindre un greffon le decable, le rallumer relance son binaire"
```

---

### Task 8 : la route HTTP

**Files:**
- Modify: `crates/ritornello-core/src/status.rs` (`OrdreGreffon`, `GreffonsControle`, `AppState`, route, handler, tests)
- Modify: `crates/ritornello-core/src/main.rs` (construction de l'`AppState`)
- Modify: `crates/ritornello-core/src/admin.rs` (constructeur d'`AppState` des tests)
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`

**Interfaces:**
- Consomme : `OrdreGreffon` (Task 7), `plugins::set_enabled` (Task 2).
- Produit : `PUT /api/plugins/:name/enabled` avec le corps `{"enabled": bool}` → `204` si accepté, `404` si le nom n'est pas déclaré, `500` si la persistance échoue ou si le cœur ne répond pas.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans `crates/ritornello-core/src/status.rs`, `mod tests` :

```rust
/// Montage avec un vrai `plugins.toml` temporaire et l'oreille du cœur
/// conservée : les deux choses que la route touche.
fn app_state_avec_greffons(
) -> (AppState, tempfile::TempDir, tokio::sync::mpsc::Receiver<OrdreGreffon>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(
        &path,
        "[[plugin]]\nname = \"radio\"\nexec = \"/bin/true\"\n\n\
         [[plugin]]\nname = \"cd\"\nexec = \"/bin/true\"\n",
    )
    .unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let state = AppState {
        greffons: Arc::new(GreffonsControle {
            manifeste: path,
            noms: vec!["radio".into(), "cd".into()],
            tx,
        }),
        ..app_state()
    };
    (state, dir, rx)
}

#[tokio::test]
async fn eteindre_persiste_puis_previent_le_coeur() {
    let (state, dir, mut rx) = app_state_avec_greffons();
    let app = router(state.clone());
    // Le cœur : il accuse réception, comme la boucle principale.
    let coeur = tokio::spawn(async move {
        let ordre = rx.recv().await.unwrap();
        assert_eq!(ordre.nom, "cd");
        assert!(!ordre.actif);
        let _ = ordre.ack.send(true);
    });

    let resp = app
        .oneshot(
            Request::put("/api/plugins/cd/enabled")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    coeur.await.unwrap();
    let apres = std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap();
    assert!(apres.contains("enabled = false"), "{apres}");
}

#[tokio::test]
async fn un_nom_non_declare_est_refuse_sans_rien_ecrire() {
    let (state, dir, _rx) = app_state_avec_greffons();
    let avant = std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap();
    let app = router(state);

    let resp = app
        .oneshot(
            Request::put("/api/plugins/jamais-vu/enabled")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Un message, jamais une clé de catalogue.
    assert!(v["error"].as_str().unwrap().contains("jamais-vu"));
    assert_eq!(std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap(), avant);
}

#[tokio::test]
async fn une_persistance_impossible_ne_touche_pas_au_runtime() {
    let (mut state, dir, mut rx) = app_state_avec_greffons();
    // Manifeste introuvable : l'écriture échouera.
    state.greffons = Arc::new(GreffonsControle {
        manifeste: dir.path().join("absent.toml"),
        noms: vec!["radio".into()],
        tx: state.greffons.tx.clone(),
    });
    let app = router(state);

    let resp = app
        .oneshot(
            Request::put("/api/plugins/radio/enabled")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    // Rien n'a été demandé au cœur : un greffon tué dont l'extinction n'est
    // pas persistée reviendrait au prochain démarrage.
    assert!(rx.try_recv().is_err());
}
```

- [ ] **Step 2: Lancer les tests pour les voir échouer**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core greffon"`
Expected: FAIL — `GreffonsControle` et le champ `greffons` n'existent pas.

- [ ] **Step 3: Déclarer le contrôle**

`OrdreGreffon` existe déjà (Task 7). Ajouter à côté, dans `crates/ritornello-core/src/status.rs` :

```rust
/// Ce que la couche HTTP doit connaître des greffons pour les basculer.
///
/// Un seul champ d'`AppState` plutôt que trois, pour la raison déjà retenue
/// pour `system` : chaque constructeur de test grossirait sinon de trois
/// lignes.
pub struct GreffonsControle {
    /// Chemin de `plugins.toml` : c'est là qu'est écrit le choix.
    pub manifeste: std::path::PathBuf,
    /// Noms déclarés, dans l'ordre du fichier. Autorité sur ce qui peut être
    /// basculé : un nom absent est refusé **avant** toute écriture.
    pub noms: Vec<String>,
    pub tx: mpsc::Sender<OrdreGreffon>,
}
```

et dans `AppState` :

```rust
    /// Bascule actif/inactif des greffons : le manifeste à réécrire, les noms
    /// acceptés, et l'oreille du cœur.
    pub greffons: Arc<GreffonsControle>,
```

- [ ] **Step 4: Écrire le handler et la route**

```rust
#[derive(Deserialize)]
struct PluginEnabledReq {
    enabled: bool,
}

/// Bascule un greffon, **persistance d'abord**.
///
/// L'ordre des trois étapes est le fond de l'affaire : un nom refusé
/// n'écrit rien, une écriture qui échoue ne tue aucun processus, et le cœur
/// n'est prévenu que d'un choix déjà sur le disque. Un greffon éteint dont
/// l'extinction n'aurait pas été écrite reviendrait au prochain démarrage —
/// un mensonge silencieux, pire qu'un refus franc.
async fn plugin_enabled_put(
    State(state): State<AppState>,
    axum::extract::Path(nom): axum::extract::Path<String>,
    Json(req): Json<PluginEnabledReq>,
) -> Response {
    let catalog = state.catalog.read().await;
    if !state.greffons.noms.iter().any(|n| n == &nom) {
        let msg = catalog.get("plugin_unknown").replace("{name}", &nom);
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    if let Err(e) = crate::plugins::set_enabled(&state.greffons.manifeste, &nom, req.enabled) {
        tracing::warn!("persisting the enabled flag of {nom}: {e:#}");
        let msg = catalog.get("plugin_persist_failed").replace("{name}", &nom);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let ordre = OrdreGreffon { nom: nom.clone(), actif: req.enabled, ack: ack_tx };
    if state.greffons.tx.send(ordre).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match ack_rx.await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        // Le cœur a refusé (binaire introuvable au rallumage) ou n'a pas
        // répondu. La cause exacte est au journal, que l'IHM montre déjà.
        _ => {
            let msg = catalog.get("plugin_action_failed").replace("{name}", &nom);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg })))
                .into_response()
        }
    }
}
```

Ajouter la route dans `router()` :

```rust
        .route("/api/plugins/:name/enabled", axum::routing::put(plugin_enabled_put))
```

**Attention** : `status.rs` utilise déjà `std::path::Path` ; écrire l'extracteur en toutes lettres (`axum::extract::Path`) comme ci-dessus plutôt que de l'importer.

- [ ] **Step 5: Ajouter les clés i18n**

Dans `crates/ritornello-core/src/locales/en.toml` :

```toml
plugin_unknown = "No plugin named '{name}' is declared in plugins.toml."
plugin_persist_failed = "Could not write the choice for '{name}' to plugins.toml; nothing was changed. See the error log."
plugin_action_failed = "The core could not apply the change for '{name}'. See the error log."
```

Dans `deploy/locales/core/fr.toml` :

```toml
plugin_unknown = "Aucun greffon nommé « {name} » n'est déclaré dans plugins.toml."
plugin_persist_failed = "Le choix pour « {name} » n'a pas pu être écrit dans plugins.toml ; rien n'a été changé. Voir le journal d'erreurs."
plugin_action_failed = "Le cœur n'a pas pu appliquer le changement pour « {name} ». Voir le journal d'erreurs."
```

- [ ] **Step 6: Compléter les constructeurs d'`AppState`**

Quatre sites de construction : `status.rs` (`app_state`, `app_state_with_audio`), `admin.rs` (`state_with`), `main.rs`. Chacun gagne une ligne. Pour les tests :

```rust
            greffons: Arc::new(GreffonsControle {
                manifeste: std::path::PathBuf::from("/nonexistent"),
                noms: Vec::new(),
                tx: tokio::sync::mpsc::channel(1).0,
            }),
```

Dans `main.rs`, avec les vraies valeurs :

```rust
        greffons: Arc::new(status::GreffonsControle {
            manifeste: plugins_path.clone(),
            noms: ordre_manifeste.clone(),
            tx: greffon_tx,
        }),
```

- [ ] **Step 7: Lancer les tests**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core"`
Expected: PASS, y compris le test de parité en/fr des catalogues.

- [ ] **Step 8: Commit**

```bash
git add crates/ritornello-core/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(core,i18n): route de bascule des greffons, persistance avant action"
```

---

### Task 9 : le tableau des greffons, une ligne par nom

**Files:**
- Modify: `web/app/src/types.ts` (`PluginStatus.disabled`)
- Modify: `web/app/src/views/ConfigView.vue`
- Modify: `web/app/src/views/ConfigView.test.ts`
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`

**Interfaces:**
- Consomme : `/api/status` avec `disabled` (Task 5), `PUT /api/plugins/:name/enabled` (Task 8).
- Produit : rien pour les tâches suivantes.

- [ ] **Step 1: Écrire les tests qui échouent**

Dans `web/app/src/views/ConfigView.test.ts`, en suivant le montage des tests existants (mock d'`api`, `mount`) :

```ts
it('regroupe les genres d un meme greffon sur une seule ligne', async () => {
  // Le tableau doit montrer l'unité qu'on manipule : la bascule porte sur le
  // greffon, pas sur un de ses genres.
  const wrapper = await monterAvecStatut({
    plugins: [
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'files', kind: 'metadata', connected: true, admin: true },
      { name: 'cd', kind: 'unknown', connected: false, admin: false, disabled: true },
    ],
    active_source: 'files',
  })
  const lignes = wrapper.findAll('[data-plugin-row]')
  expect(lignes).toHaveLength(2)
  expect(lignes[0].find('[data-plugin-kind]').text()).toBe('source, metadata')
})

it('bascule un greffon et recharge', async () => {
  const wrapper = await monterAvecStatut({
    plugins: [{ name: 'cd', kind: 'source', connected: true, admin: false }],
    active_source: 'cd',
  })
  await wrapper.find('[data-plugin-toggle]').trigger('click')
  await flushPromises()
  expect(api.put).toHaveBeenCalledWith('/api/plugins/cd/enabled', { enabled: false })
})

it('dit pourquoi quand le coeur refuse', async () => {
  vi.mocked(api.put).mockResolvedValueOnce('plugins.toml est en lecture seule')
  const wrapper = await monterAvecStatut({
    plugins: [{ name: 'cd', kind: 'source', connected: true, admin: false }],
    active_source: 'cd',
  })
  await wrapper.find('[data-plugin-toggle]').trigger('click')
  await flushPromises()
  expect(toast.error).toHaveBeenCalledWith('plugins.toml est en lecture seule')
})
```

`monterAvecStatut(statut)` : helper local qui pose la réponse de `api.get('/api/status')` puis monte la vue — reprendre le montage déjà écrit dans ce fichier plutôt que d'en inventer un.

- [ ] **Step 2: Lancer les tests pour les voir échouer**

Run, depuis `web/app` : `npx vitest run src/views/ConfigView.test.ts`
Expected: FAIL — trois lignes rendues au lieu de deux, pas de `[data-plugin-toggle]`.

- [ ] **Step 3: Ajouter `disabled` au type**

Dans `web/app/src/types.ts` :

```ts
export interface PluginStatus { name: string; kind: string; connected: boolean; admin: boolean; stalled?: boolean; disabled?: boolean }
```

- [ ] **Step 4: Regrouper par nom dans la vue**

Dans le `<script setup>` de `web/app/src/views/ConfigView.vue`, ajouter l'import `Switch` à la liste `@ritornello/ui`, puis :

```ts
interface LigneGreffon {
  name: string
  kinds: string
  connected: boolean
  stalled: boolean
  disabled: boolean
  admin: boolean
}

/**
 * Une ligne par greffon, ses genres joints. Le tableau montrait un couple
 * (nom, genre) par ligne ; la bascule porte sur le nom, et trois interrupteurs
 * qui font tous la même chose ne veulent rien dire.
 *
 * Un greffon n'est « connecté » que si **tous** ses genres le sont : une
 * moitié injoignable est un problème, et l'agrégat ne doit pas la cacher.
 */
const greffons = computed<LigneGreffon[]>(() => {
  const parNom = new Map<string, LigneGreffon>()
  for (const p of status.value.plugins) {
    const ligne = parNom.get(p.name)
    if (!ligne) {
      parNom.set(p.name, {
        name: p.name,
        kinds: p.kind,
        connected: p.connected,
        stalled: !!p.stalled,
        disabled: !!p.disabled,
        admin: p.admin,
      })
      continue
    }
    // « unknown » n'est pas un genre à afficher à côté d'un vrai.
    ligne.kinds = ligne.kinds === 'unknown' ? p.kind : `${ligne.kinds}, ${p.kind}`
    ligne.connected = ligne.connected && p.connected
    ligne.stalled = ligne.stalled || !!p.stalled
    ligne.disabled = ligne.disabled || !!p.disabled
    ligne.admin = ligne.admin || p.admin
  }
  return [...parNom.values()]
})

async function basculerGreffon(ligne: LigneGreffon) {
  const actif = ligne.disabled
  const err = await api.put(`/api/plugins/${encodeURIComponent(ligne.name)}/enabled`, {
    enabled: actif,
  })
  if (err) {
    toast.error(err)
  } else {
    toast.success(t.value(actif ? 'plugin_enabled' : 'plugin_disabled', { name: ligne.name }))
  }
  // Rechargement dans les deux cas : un refus a pu laisser l'état d'avant, et
  // un succès change les lignes de plusieurs genres à la fois.
  await chargerTout()
}
```

- [ ] **Step 5: Réécrire le tableau**

Remplacer le `<table>` de la section `#plugins` :

```html
            <table class="w-full text-sm">
              <thead class="text-muted-foreground">
                <tr>
                  <th class="text-left font-normal">{{ t('col_plugin') }}</th>
                  <th class="text-left font-normal">{{ t('col_kind') }}</th>
                  <th class="text-left font-normal">{{ t('col_state') }}</th>
                  <th class="text-left font-normal">{{ t('col_admin') }}</th>
                  <th class="text-left font-normal">{{ t('col_enabled') }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="p in greffons" :key="p.name" data-plugin-row class="border-t border-border">
                  <td class="py-1" data-plugin-name>{{ p.name }}</td>
                  <td data-plugin-kind>{{ p.kinds }}</td>
                  <td data-plugin-state>
                    <Badge
                      :variant="p.disabled ? 'outline' : p.connected ? 'secondary' : p.stalled ? 'outline' : 'destructive'"
                    >
                      {{ p.disabled ? t('disabled') : p.connected ? t('connected') : p.stalled ? t('stalled') : t('unavailable') }}
                    </Badge>
                  </td>
                  <td>
                    <RouterLink v-if="p.admin" :to="`/plugins/${p.name}/`" data-admin-link class="underline">
                      {{ t('admin_link') }}
                    </RouterLink>
                    <span v-else>-</span>
                  </td>
                  <td>
                    <!-- Pas de confirmation : l'action est réversible depuis
                         cette même ligne, et la notification dit ce qui s'est
                         passé. -->
                    <Switch
                      data-plugin-toggle
                      :model-value="!p.disabled"
                      :aria-label="t('toggle_plugin', { name: p.name })"
                      @click="basculerGreffon(p)"
                    />
                  </td>
                </tr>
              </tbody>
            </table>
```

- [ ] **Step 6: Ajouter les clés i18n**

`crates/ritornello-core/src/locales/en.toml` :

```toml
col_enabled = "Enabled"
disabled = "disabled"
toggle_plugin = "Enable or disable {name}"
plugin_enabled = "{name} enabled."
plugin_disabled = "{name} disabled."
```

`deploy/locales/core/fr.toml` :

```toml
col_enabled = "Actif"
disabled = "désactivé"
toggle_plugin = "Activer ou désactiver {name}"
plugin_enabled = "{name} activé."
plugin_disabled = "{name} désactivé."
```

- [ ] **Step 7: Lancer les tests web puis Rust**

Run, depuis `web/app` : `npx vitest run`
Expected: PASS, `i18nKeysUsed.test.ts` compris.

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core parite_des_cles"`
Expected: PASS — la parité en/fr couvre les cinq nouvelles clés. (Le filtre `locales` ne vise **pas** ce test : il ne matche qu'un `parse_available_locales_*` sans rapport. Viser le test de parité par son nom, ou lancer la suite du paquet.)

- [ ] **Step 8: Commit**

```bash
git add web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): une ligne par greffon et un interrupteur actif/inactif"
```

---

### Task 10 : le cycle complet, et la documentation

**Files:**
- Modify: `crates/ritornello-core/src/plugins.rs` (test d'aller-retour)
- Modify: `docs/plugins.md`
- Modify: `deploy/plugins.example.toml`

**Interfaces:**
- Consomme : tout ce qui précède.
- Produit : rien.

- [ ] **Step 1: Écrire le test de cycle**

Le test couvre l'aller-retour **fichier → manifeste → fichier**, seul maillon que les tâches précédentes n'ont vu que par moitiés. Le câblage est couvert ailleurs : `register` pour l'annonce et le recâblage, `remove_source` pour le décâblage, la route pour l'enchaînement persistance-puis-ordre. La boucle `main` elle-même n'est pas testable en l'état — un test « bout en bout » l'exigerait extraite, ce que ce chantier ne fait pas.

À ajouter dans le `mod tests` de `crates/ritornello-core/src/plugins.rs`, où vivent `manifeste_commente` et `set_enabled` :

```rust
#[test]
fn eteint_puis_rallume_le_greffon_retrouve_sa_place_dans_le_fichier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins.toml");
    std::fs::write(&path, manifeste_commente()).unwrap();

    set_enabled(&path, "musicbrainz", false).unwrap();
    let m = PluginManifest::load(&path).unwrap();
    assert!(m.plugins[0].enabled, "le voisin reste allumé");
    assert!(!m.plugins[1].enabled);

    set_enabled(&path, "musicbrainz", true).unwrap();
    let m = PluginManifest::load(&path).unwrap();
    assert!(m.plugins.iter().all(|p| p.enabled), "tout est rallumé");
    // L'ordre du fichier arbitre les `metadata` : un greffon rallumé doit
    // reprendre sa priorité d'origine, pas la queue de liste.
    assert_eq!(
        m.plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["radio", "musicbrainz"]
    );
    // Et le fichier est revenu à sa forme d'origine, commentaires compris.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), manifeste_commente());
}
```

Si la dernière assertion échoue sur un détail de mise en forme que `toml_edit` normalise (une ligne vide finale, par exemple), **ne pas la supprimer** : la remplacer par les deux vérifications qu'elle porte — les deux commentaires présents, et aucune occurrence de `enabled`.

- [ ] **Step 2: Lancer le test**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test -p ritornello-core eteint_puis_rallume"`
Expected: PASS.

- [ ] **Step 3: Documenter dans `docs/plugins.md`**

Dans la section « Declaring the plugins », après la description des deux clés, ajouter une sous-section **« Turning a plugin off »** (en anglais, comme le reste du fichier) qui dit :

- une troisième clé, optionnelle, `enabled` ; absente vaut actif ;
- la bascule se fait depuis la page de configuration, pas à la main, et le
  cœur réécrit le fichier en préservant ses commentaires ;
- éteindre **tue le processus** — c'est ce qui rend `/dev/sr0`, l'evdev ou la
  console ; rallumer relance le binaire, qui s'annonce et est recâblé sans
  redémarrage du cœur ;
- éteindre la source active bascule sur la suivante, ou laisse l'appareil sans
  source — un état légitime ;
- tout éteindre ne fait pas échouer le démarrage : sinon l'IHM disparaîtrait
  avec le reste, et plus rien ne pourrait être rallumé ;
- l'ordre du fichier continue d'arbitrer les `metadata`, greffons éteints
  compris : un greffon rallumé retrouve sa priorité de fichier.

- [ ] **Step 4: Un mot dans `deploy/plugins.example.toml`**

En tête du fichier, avant le premier `[[plugin]]` :

```toml
# Chaque entrée n'a besoin que de `name` et `exec`. Une troisième clé,
# `enabled`, apparaît quand un greffon est éteint depuis la page de
# configuration ; son absence vaut actif. Elle se bascule depuis l'IHM — le
# cœur réécrit ce fichier en préservant ces commentaires.
```

- [ ] **Step 5: Suite complète**

Run : `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/greffons-actif-inactif && cargo test"`
Run, depuis `web/app` : `npx vitest run`
Expected: PASS des deux côtés.

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-core docs/plugins.md deploy/plugins.example.toml
git commit -m "test,docs(plugins): cycle eteindre-rallumer et documentation de la cle enabled"
```

---

## Ce que ce plan ne fait pas

- désactivation par genre plutôt que par greffon ;
- ordre de démarrage réglable depuis l'IHM ;
- redémarrer un greffon sans passer par éteindre puis rallumer ;
- relecture de `plugins.toml` modifié à la main pendant que le cœur tourne
  (la table des `exec` est celle du démarrage).
