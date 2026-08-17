# Position dans la piste, durée, et touches de déplacement — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publier la durée et la position de ce qui joue vers la SPA et les plugins d'affichage, et ajouter deux touches (± un pas réglable) plus une barre cliquable pour se déplacer dans la piste.

**Architecture :** Deux champs additifs dans la charge utile unique `PlayerState` (`position_s`, `seekable`), alimentés par deux fournisseurs qui ne se disputent jamais — mpv pour un contenu fini, un plugin `metadata` pour un flux. Un tick d'une seconde dans le `select!` de `main.rs` publie l'état tant qu'une position est connue, sans jamais toucher aux échéances d'incrustation. Trois commandes de protocole (`SeekForward`, `SeekBackward`, `SeekTo`) traduites en `seek` mpv, ignorées en silence hors contenu déplaçable.

**Tech Stack :** Rust (tokio, serde, axum), Vue 3 + TypeScript (vitest, Playwright), mpv IPC JSON.

**Spec :** `docs/superpowers/specs/2026-08-17-position-et-duree-design.md`

## Global Constraints

- **Toute commande cargo passe par WSL.** `cargo` n'existe pas côté Windows. Préfixe obligatoire :
  `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && <commande>"`
  Les commandes `npm` s'exécutent normalement, côté Windows, depuis la racine du dépôt.
- **Commentaires de code en français, messages de journal (`tracing`) en anglais.** Convention du dépôt, vérifiée par un test pour les messages destinés à l'écran.
- **Tout message atteignant l'utilisateur passe par un catalogue i18n.** Jamais de chaîne en dur dans une réponse HTTP ni dans un libellé d'IHM. Catalogue de référence : `crates/ritornello-core/src/locales/en.toml`, traduction française : `deploy/locales/core/fr.toml`. Les deux fichiers doivent recevoir chaque nouvelle clé.
- **Défauts et bornes, valeurs exactes :** `seek_step_s` défaut **10**, bornes **1..=120** secondes.
- **Champs de protocole additifs :** tout nouveau champ est `#[serde(default)]` et n'est pas sérialisé quand il ne dit rien (`skip_serializing_if`), pour qu'une trame ancienne reste lisible et qu'une trame muette reste identique à l'octet près.
- **TDD strict :** le test d'abord, on le voit échouer, puis l'implémentation minimale, puis on le voit passer, puis on commit.
- **Un commit par tâche**, message en français, sans mention d'outil ni de co-auteur.

### Le montage de test du cœur, tel qu'il existe déjà

À connaître avant d'écrire le moindre test dans `crates/ritornello-core/src/core.rs` — ces assistants existent, il ne faut **pas** en créer d'autres :

- `fn setup() -> Montage` — **synchrone**, pas de `.await`. Rend le quintuplet `(core, player_calls, source_calls, etat_rx, dir)`. Deux sources factices, `cd` et `radio` ; **la source active au départ est `radio`**, parce que `PersistedState::default()` la déclare ainsi (`state.rs`) — et non `cd`, que l'ordre trié laisserait croire.
- `fn setup_metadonnees(plugins: Vec<String>) -> (Core<FakePlayer>, watch::Receiver<NowPlaying>, watch::Receiver<PlayerState>, TempDir)` — **synchrone** elle aussi, quadruplet.
- `fn joue(identity: Value) -> SourceUpdate` et `fn update_nu() -> SourceUpdate` — les trames de source ; l'identité s'installe par `core.handle_source_update("cd", joue(id))`.
- `fn enrichissement(identity, artist, title) -> Enrichment`.
- **Faire jouer un flux** (`expecting_stream` vrai, contenu non déplaçable) : la source active étant déjà `radio`, `core.handle_command(Command::PlayPause).await.unwrap()` suffit — elle répond `play("http://fip")` sans `finite`.
- **Faire jouer un contenu fini** (déplaçable, mpv a la parole) : `core.handle_command(Command::SourceCycle).await.unwrap()` bascule de `radio` vers `cd`, qui répond `play("cdda://").finite()`.
- Ces deux idiomes sont l'inverse l'un de l'autre et faciles à confondre : le flux est l'état par défaut, le contenu fini demande une bascule.
- `PlayerState.duration_s` n'existe **pas** en accès direct : le champ vit dans `PlayerState.morceau.duration_s` (`serde(flatten)` aplatit le JSON, pas la structure Rust). Écrire `etat.duration_s` ne compile pas.
- Il n'existe **pas** de `set_active_source` ni de `metadonnees_identity` : le module `tests` est un enfant du module `core`, donc `core.metadonnees` et les champs privés lui sont directement accessibles.
- Les catalogues de test se construisent par `ritornello_i18n::Catalog::load("core", "en", std::path::Path::new("/inexistant"), crate::core::EN)` — le chemin inexistant force le repli sur le catalogue anglais embarqué. Il n'existe pas de `Catalog::from_pairs`.

---

## Tâche 0 : mesure matérielle préalable (propriétaire)

Cette tâche n'écrit pas de code. Elle lève la seule inconnue du design, et **ne bloque que la tâche 15**. Toutes les autres tâches peuvent avancer sans elle.

**Ce qu'il faut mesurer**, sur le Pi, disque audio dans le tiroir, en jouant la piste 3 :

```bash
# Depuis la machine où tourne ritornello, socket mpv du service :
echo '{"command":["get_property","time-pos"]}'  | socat - /run/ritornello/mpv.sock
echo '{"command":["get_property","duration"]}'  | socat - /run/ritornello/mpv.sock
echo '{"command":["get_property","chapter"]}'   | socat - /run/ritornello/mpv.sock
echo '{"command":["get_property","chapter-list"]}' | socat - /run/ritornello/mpv.sock
```

**La question :** dix secondes après le début de la piste 3, `time-pos` vaut-il ~10 (relatif à la piste) ou la somme des pistes précédentes + 10 (relatif au disque) ? Et `duration` : la piste ou le disque entier ?

**Le second point à mesurer**, radio en cours de lecture :

```bash
echo '{"command":["get_property","duration"]}' | socat - /run/ritornello/mpv.sock
```

mpv annonce-t-il une durée absurde sur un flux ? (La garde `finite` la couvre déjà par construction ; il s'agit de confirmer, pas de supposer.)

- [ ] **Étape 1 : consigner le résultat**

Écrire les valeurs relevées dans `docs/superpowers/plans/2026-08-17-position-et-duree.md`, en remplaçant cette étape par le relevé, puis committer. Si `time-pos` est relatif au **disque**, la tâche 11 devient obligatoire ; s'il est relatif à la **piste**, la tâche 11 est supprimée du plan.

---

## Structure des fichiers

**Protocole** (`crates/ritornello-proto/src/`)
- `metadata.rs` — `PlayerState` gagne `position_s` et `seekable` ; `Enrichment` gagne `position_s`.
- `command.rs` — `Command` gagne `SeekForward`, `SeekBackward`, `SeekTo(u32)`.

**Cœur** (`crates/ritornello-core/src/`)
- `player/mod.rs` — le trait `Player` gagne `progression()`, `seek_relative()`, `seek_absolute()` ; nouveau type `Progression`.
- `player/mpv.rs` — leur implémentation par `get_property` / `seek`.
- `core.rs` — champs de position, précédence de durée, `seekable`, rafraîchissement, traitement des trois commandes.
- `metadata.rs` — `Metadonnees::position_s()`, lecture pure du gagnant.
- `main.rs` — le bras de tick dans le `select!`.
- `state.rs` — `Settings::seek_step_s`.
- `status.rs` — bornes et message d'erreur du réglage.
- `locales/en.toml` — clés nouvelles.

**Plugins**
- `crates/ritornello-plugin-radiofrance-metas/src/live.rs` et `main.rs` — `start_time` puis `position_s`.
- `crates/ritornello-plugin-generic-input/ui/src/preset-toml.ts` et `src/locales/en.toml` — deux actions apprenables.

**SPA** (`web/app/src/`)
- `types.ts` — `PlayerPayload` et `SettingsPayload`.
- `composables/usePlayer.ts` — `formatePosition`.
- `components/BarreProgression.vue` — nouveau composant.
- `components/PlayerCard.vue` — l'assemble.
- `views/remoteCommands.ts` — deux boutons.
- `views/ConfigView.vue` — la carte de réglage.

**Traductions et docs**
- `deploy/locales/core/fr.toml`, `deploy/locales/generic-input/fr.toml`.
- `docs/interface.md`, `docs/plugins.md`.

---

## Tâche 1 : le protocole apprend la position et le déplacement

**Files:**
- Modify: `crates/ritornello-proto/src/metadata.rs`
- Modify: `crates/ritornello-proto/src/command.rs`

**Interfaces:**
- Consomme : rien.
- Produit : `PlayerState.position_s: Option<u32>`, `PlayerState.seekable: bool`, `Enrichment.position_s: Option<u32>`, `Command::SeekForward`, `Command::SeekBackward`, `Command::SeekTo(u32)`.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `crates/ritornello-proto/src/metadata.rs`, module `tests`, ajouter :

```rust
    #[test]
    fn player_state_serialise_position_et_seekable_quand_ils_disent_quelque_chose() {
        let etat = PlayerState {
            source: "cd".into(),
            position_s: Some(87),
            seekable: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(json.contains(r#""position_s":87"#), "{json}");
        assert!(json.contains(r#""seekable":true"#), "{json}");
    }

    /// Additif : une trame muette sur ces deux champs reste identique à
    /// l'octet près à ce qu'elle était avant ce chantier, et une trame
    /// écrite par un binaire antérieur se relit sans eux.
    #[test]
    fn player_state_tait_position_et_seekable_quand_ils_ne_disent_rien() {
        let etat = PlayerState { source: "radio".into(), ..Default::default() };
        let json = serde_json::to_string(&etat).unwrap();
        assert!(!json.contains("position_s"), "{json}");
        assert!(!json.contains("seekable"), "{json}");
        let ancienne = r#"{"source":"radio","volume":50,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#;
        let relue: PlayerState = serde_json::from_str(ancienne).unwrap();
        assert_eq!(relue.position_s, None);
        assert!(!relue.seekable);
    }

    #[test]
    fn enrichment_porte_une_position() {
        let e = Enrichment {
            identity: json!({"kind": "stream"}),
            position_s: Some(42),
            ..Default::default()
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.position_s, Some(42));
        let sans = r#"{"identity":{"kind":"stream"}}"#;
        assert_eq!(serde_json::from_str::<Enrichment>(sans).unwrap().position_s, None);
    }
```

Dans `crates/ritornello-proto/src/command.rs`, module `tests`, ajouter :

```rust
    #[test]
    fn roundtrip_des_commandes_de_deplacement() {
        for (cmd, attendu) in [
            (Command::SeekForward, r#"{"cmd":"SeekForward"}"#),
            (Command::SeekBackward, r#"{"cmd":"SeekBackward"}"#),
            (Command::SeekTo(198), r#"{"cmd":"SeekTo","arg":198}"#),
        ] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(json, attendu);
            assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
        }
    }
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-proto"`
Expected: FAIL — `no field 'position_s' on type 'PlayerState'`, `no variant named 'SeekForward'`.

- [ ] **Étape 3 : ajouter les champs**

Dans `metadata.rs`, dans `struct PlayerState`, **après** le champ `overlay` et **avant** `morceau` :

```rust
    /// Où en est ce qui joue, en secondes, **à l'instant de la publication**.
    ///
    /// `None` = personne n'a de quoi répondre : rien ne joue, ou c'est un flux
    /// que nul plugin `metadata` ne suit. Deux fournisseurs alimentent ce
    /// champ sans jamais se disputer — mpv pour un contenu fini, un plugin
    /// `metadata` pour un flux — parce que le contexte décide lequel des deux
    /// a le droit de parler (voir `Core::rafraichit_position`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_s: Option<u32>,
    /// Ce qui joue accepte un déplacement : c'est le `finite` que la Source a
    /// déclaré à son `Play`, rendu visible aux consommateurs.
    ///
    /// Un champ à part entière plutôt qu'une déduction de `duration_s` : les
    /// deux notions divergent exactement là où ça compte — Radio France
    /// annonce la durée d'un morceau sur un direct qu'on ne peut pas
    /// rembobiner, un fichier sans étiquette de durée reste parcourable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seekable: bool,
```

Dans `struct Enrichment`, après `duration_s` :

```rust
    /// Écoulé dans le morceau **au moment de l'émission**, en secondes.
    ///
    /// Un écoulé relatif plutôt qu'un horodatage absolu : rien à synchroniser
    /// entre deux horloges, et c'est la convention de `duration_s` juste
    /// au-dessus. Le cœur l'ancre à la réception et l'avance lui-même ensuite
    /// (voir `Core::rafraichit_position`).
    #[serde(default)]
    pub position_s: Option<u32>,
```

Dans `command.rs`, dans `enum Command`, après `Plus10` :

```rust
    /// Avancer d'un pas dans ce qui joue. Le pas vit dans le cœur (réglage
    /// `seek_step_s`), exactement comme les 5 % du volume : la touche ne
    /// porte aucune quantité, donc changer le pas ne demande pas de
    /// reprogrammer une télécommande.
    SeekForward,
    SeekBackward,
    /// Positionnement absolu, en secondes. Sert la barre cliquable de la SPA ;
    /// aucune touche physique ne l'émet.
    SeekTo(u32),
```

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-proto"`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-proto/src/metadata.rs crates/ritornello-proto/src/command.rs
git commit -m "feat(proto): la charge utile porte la position, et trois commandes de deplacement"
```

---

## Tâche 2 : le lecteur sait se situer et se déplacer

**Files:**
- Modify: `crates/ritornello-core/src/player/mod.rs`
- Modify: `crates/ritornello-core/src/player/mpv.rs`
- Modify: `crates/ritornello-core/src/core.rs` (le `FakePlayer` des tests, vers la ligne 1059)

**Interfaces:**
- Consomme : rien de la tâche 1.
- Produit : `player::Progression { position_s: Option<f64>, duration_s: Option<f64> }` ; `Player::progression(&self) -> Result<Progression>` ; `Player::seek_relative(&self, delta_s: i64) -> Result<()>` ; `Player::seek_absolute(&self, position_s: u32) -> Result<()>`.

- [ ] **Étape 1 : écrire le test qui échoue**

Dans `crates/ritornello-core/src/player/mpv.rs`, module `tests` (le créer en fin de fichier s'il n'existe pas) :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// mpv répond `null` sur `time-pos` quand rien n'est chargé, et une
    /// **erreur** quand la propriété n'est pas disponible. Les deux disent la
    /// même chose — « je ne sais pas » — et aucune n'est une panne à faire
    /// remonter : une position inconnue est un cas normal, pas un incident.
    #[test]
    fn une_valeur_absente_ou_nulle_devient_none() {
        assert_eq!(nombre_ou_none(Ok(serde_json::json!(87.4))), Some(87.4));
        assert_eq!(nombre_ou_none(Ok(serde_json::Value::Null)), None);
        assert_eq!(nombre_ou_none(Err(anyhow::anyhow!("property unavailable"))), None);
    }

    /// Une position négative n'existe pas, et mpv en produit brièvement au
    /// démarrage d'un fichier (mesuré : `-0.02`). La publier ferait afficher
    /// une barre qui recule.
    #[test]
    fn une_valeur_negative_devient_none() {
        assert_eq!(nombre_ou_none(Ok(serde_json::json!(-0.02))), None);
    }
}
```

- [ ] **Étape 2 : voir le test échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core nombre_ou_none"`
Expected: FAIL — `cannot find function 'nombre_ou_none'`.

- [ ] **Étape 3 : implémenter**

Dans `crates/ritornello-core/src/player/mod.rs`, ajouter avant le trait :

```rust
/// Où en est la lecture et combien elle dure, telles que le lecteur les
/// connaît à cet instant.
///
/// Les deux ensemble et non deux méthodes : elles sont lues au même moment,
/// pour la même trame, et un appelant qui n'en prendrait qu'une publierait un
/// couple incohérent (une position d'une piste, la durée de la suivante).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progression {
    pub position_s: Option<f64>,
    pub duration_s: Option<f64>,
}
```

et dans le trait `Player`, après `set_audio_device` :

```rust
    /// Position et durée courantes. `Ok` avec des champs à `None` quand le
    /// lecteur ne sait pas : une position inconnue est un cas normal (rien
    /// n'est chargé, le flux n'a pas de durée), jamais une panne.
    async fn progression(&self) -> Result<Progression>;
    /// Déplacement relatif, en secondes (négatif pour reculer).
    async fn seek_relative(&self, delta_s: i64) -> Result<()>;
    /// Déplacement absolu, en secondes depuis le début.
    async fn seek_absolute(&self, position_s: u32) -> Result<()>;
```

Dans `crates/ritornello-core/src/player/mpv.rs`, ajouter la fonction pure (au niveau du module, avant `impl Player for MpvPlayer`) :

```rust
/// Ramène une réponse de `get_property` à un nombre utilisable.
///
/// Trois façons pour mpv de dire « je ne sais pas », toutes ramenées à
/// `None` : l'erreur (`property unavailable` sur un flux sans durée), le
/// `null`, et la valeur négative que mpv produit brièvement au démarrage d'un
/// fichier — mesuré à `-0.02`, et publier cela ferait reculer la barre.
fn nombre_ou_none(res: Result<Value>) -> Option<f64> {
    res.ok().and_then(|v| v.as_f64()).filter(|n| *n >= 0.0)
}
```

et l'implémentation, dans `impl Player for MpvPlayer` :

```rust
    async fn progression(&self) -> Result<Progression> {
        // Deux allers-retours par seconde sur une socket Unix locale : le coût
        // est nul devant l'intervalle. Un sondage plutôt qu'un
        // `observe_property` parce que mpv ne cadence pas ses notifications de
        // `time-pos` — il en émettrait plusieurs par seconde pour une
        // information publiée une fois par seconde.
        let position = self.ipc.command(&[json!("get_property"), json!("time-pos")]).await;
        let duree = self.ipc.command(&[json!("get_property"), json!("duration")]).await;
        Ok(Progression { position_s: nombre_ou_none(position), duration_s: nombre_ou_none(duree) })
    }

    async fn seek_relative(&self, delta_s: i64) -> Result<()> {
        self.ipc
            .command(&[json!("seek"), json!(delta_s), json!("relative")])
            .await
            .map(|_| ())
    }

    async fn seek_absolute(&self, position_s: u32) -> Result<()> {
        self.ipc
            .command(&[json!("seek"), json!(position_s), json!("absolute")])
            .await
            .map(|_| ())
    }
```

Ajouter `use crate::player::Progression;` en haut de `mpv.rs` si l'import n'est pas déjà couvert.

Dans `crates/ritornello-core/src/core.rs`, module `tests`, compléter `FakePlayer`. Remplacer sa déclaration par :

```rust
    #[derive(Default)]
    struct FakePlayer {
        calls: Arc<Mutex<Vec<String>>>,
        /// Ce que le lecteur factice prétend savoir de sa progression.
        /// `Mutex` et non champ simple : les tests le règlent après
        /// construction, `Player` ne prenant que `&self`.
        progression: Arc<Mutex<crate::player::Progression>>,
    }
```

et ajouter à son `impl Player` :

```rust
        async fn progression(&self) -> anyhow::Result<crate::player::Progression> {
            Ok(*self.progression.lock().unwrap())
        }
        async fn seek_relative(&self, delta_s: i64) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("seek_relative {delta_s}"));
            Ok(())
        }
        async fn seek_absolute(&self, position_s: u32) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!("seek_absolute {position_s}"));
            Ok(())
        }
```

Adapter les constructions existantes de `FakePlayer` dans les tests : là où le code écrit `FakePlayer { calls: calls.clone() }`, écrire `FakePlayer { calls: calls.clone(), ..Default::default() }`.

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core"`
Expected: PASS — toute la suite du cœur, pas seulement le nouveau test.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-core/src/player/ crates/ritornello-core/src/core.rs
git commit -m "feat(core): le lecteur sait dire ou il en est et s y deplacer"
```

---

## Tâche 3 : la position entre dans l'état publié

**Files:**
- Modify: `crates/ritornello-core/src/core.rs`

**Interfaces:**
- Consomme : `PlayerState.position_s` / `.seekable` (tâche 1), `Player::progression` et `Progression` (tâche 2).
- Produit : champs `Core.position_s: Option<u32>`, `Core.duree_mesuree_s: Option<u32>` ; méthode `pub async fn rafraichit_position(&mut self)` ; méthode `fn oublie_position(&mut self)`.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `crates/ritornello-core/src/core.rs`, module `tests` :

```rust
    #[tokio::test]
    async fn la_position_de_mpv_est_publiee_sur_un_contenu_fini() {
        // La source active de `setup()` est `cd` : `PlayPause` sans rien qui
        // joue redemande à la source d'activer, et la factice répond
        // `play("cdda://").finite()`.
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::PlayPause).await.unwrap();
        core.regle_progression(Some(87.4), Some(254.0));
        core.rafraichit_position().await;
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, Some(87), "tronquée, jamais arrondie au-dessus");
        // 87.6 et non 87.4 : au-dessus de la demi-seconde, une troncature et un
        // arrondi ne donnent plus le même entier, et le test distingue enfin
        // les deux implémentations.
        core.regle_progression(Some(87.6), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        assert_eq!(etat.duration_s, Some(254));
        assert!(etat.seekable, "un disque se parcourt");
    }

    /// Sur un flux, `time-pos` compte depuis le début de la connexion et n'a
    /// aucun rapport avec le morceau : il est lu et jeté. Sans cette garde, la
    /// radio afficherait un compteur d'écoute croissant à la place de la
    /// position dans le morceau.
    #[tokio::test]
    async fn la_position_de_mpv_est_ecartee_sur_un_flux() {
        let (mut core, _, _, _, _dir) = setup();
        // Bascule vers `radio`, qui répond `play("http://fip")` sans `finite`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.regle_progression(Some(1234.0), Some(0.0));
        core.rafraichit_position().await;
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, None);
        assert!(!etat.seekable, "un direct ne se rembobine pas");
    }

    #[tokio::test]
    async fn l_arret_oublie_la_position() {
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::PlayPause).await.unwrap();
        core.regle_progression(Some(87.0), Some(254.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        core.handle_command(Command::Stop).await.unwrap();
        let etat = core.etat_lecteur();
        assert_eq!(etat.position_s, None, "plus rien ne joue, plus rien à situer");
        assert_eq!(etat.duration_s, None);
        assert!(!etat.seekable);
    }

    /// La durée mesurée par mpv l'emporte sur celle qu'un plugin annonce : le
    /// disque réel prime sur ce qu'une base en ligne en dit.
    #[tokio::test]
    async fn la_duree_de_mpv_l_emporte_sur_celle_d_un_plugin() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["musicbrainz".into()]);
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"disc": "abc", "track": 2});
        core.handle_source_update("cd", joue(id.clone()));
        core.handle_enrichment(
            "musicbrainz",
            Enrichment {
                identity: id,
                title: Some("So What".into()),
                duration_s: Some(999),
                ..Default::default()
            },
        );
        core.regle_progression(Some(10.0), Some(545.0));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().duration_s, Some(545));
    }
```

Ajouter aussi, dans le module `tests`, l'assistant de réglage du lecteur factice (méthode de test sur `Core`, à placer près des autres assistants) :

```rust
    impl Core<FakePlayer> {
        /// Règle ce que le lecteur factice prétend savoir de sa progression.
        fn regle_progression(&self, position_s: Option<f64>, duration_s: Option<f64>) {
            *self.player.progression.lock().unwrap() =
                crate::player::Progression { position_s, duration_s };
        }
    }
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core position"`
Expected: FAIL — `no method named 'rafraichit_position'`.

- [ ] **Étape 3 : implémenter**

Dans `struct Core<P: Player>`, après le champ `volume_deadline` :

```rust
    /// Où en est ce qui joue, en secondes entières, tel que le dernier
    /// rafraîchissement l'a établi. Publié tel quel par `etat_lecteur`.
    position_s: Option<u32>,
    /// Durée **mesurée par mpv**, distincte de celle qu'un plugin `metadata`
    /// annonce. Gardée à part parce qu'elle la supplante : les fondre en un
    /// seul champ ferait perdre la trace de qui a parlé, et la précédence
    /// deviendrait un ordre d'écriture — le genre d'invariant qui se casse en
    /// silence.
    duree_mesuree_s: Option<u32>,
```

Les initialiser à `None` dans `Core::new`.

Ajouter, près de `publie_etat` :

```rust
    /// Relit où on en est, auprès du fournisseur qui a le droit de parler.
    ///
    /// Deux fournisseurs, jamais en concurrence : mpv pour un contenu fini,
    /// un plugin `metadata` pour un flux. Le `time-pos` d'un flux compte
    /// depuis le début de la connexion et n'a aucun rapport avec le morceau —
    /// il est lu et jeté, jamais publié.
    ///
    /// Ne publie rien : l'appelant décide (le tick publie, `handle_command`
    /// publie déjà en sortie).
    pub async fn rafraichit_position(&mut self) {
        if self.standby || !self.lecture {
            self.oublie_position();
            return;
        }
        if self.expecting_stream {
            // Flux : mpv ne sait rien d'utile. La position viendra de l'ancre
            // d'un plugin `metadata` (tâche 5) ou de nulle part.
            //
            // Les DEUX champs sont remis à zéro, et c'est un défaut trouvé en
            // relecture : `lecture` reste vrai d'un bout à l'autre d'un
            // changement de source (le cœur le repose aussitôt), si bien
            // qu'une position mesurée sur un disque survivait au passage à la
            // radio et s'affichait indéfiniment sous le flux. Le premier
            // garde-fou (`!self.lecture`) ne se déclenche jamais dans cette
            // séquence.
            //
            // `self.position_s = None` et non `self.oublie_position()` : cette
            // dernière effacera aussi l'ancre en tâche 5, or c'est précisément
            // l'ancre qui doit survivre ici.
            self.position_s = None;
            self.duree_mesuree_s = None;
            return;
        }
        match self.player.progression().await {
            Ok(p) => {
                self.position_s = p.position_s.map(|s| s as u32);
                self.duree_mesuree_s = p.duration_s.filter(|d| *d > 0.0).map(|s| s as u32);
            }
            Err(e) => {
                // Une position illisible n'arrête pas la musique : on cesse
                // simplement d'en annoncer une.
                tracing::debug!("playback progress unavailable: {e}");
                self.position_s = None;
                self.duree_mesuree_s = None;
            }
        }
    }

    /// Plus rien ne joue : plus rien à situer.
    fn oublie_position(&mut self) {
        self.position_s = None;
        self.duree_mesuree_s = None;
    }
```

Dans `etat_lecteur`, remplacer la ligne `morceau: self.metadonnees.etat(),` par :

```rust
            // Gardée **ici**, à la publication, et non effacée dans chacun des
            // cinq chemins qui posent `lecture = false` (arrêt, veille,
            // changement de source, fin de contenu, `SourceAction::Stop`).
            // Un point unique ne peut pas être oublié ; cinq appels
            // sprinkled le seraient au sixième chemin ajouté, et la barre
            // resterait figée sur la dernière valeur connue sans que rien ne
            // le signale.
            position_s: if self.lecture && !self.standby { self.position_s } else { None },
            // `lecture` et non `expecting_stream` : la première dit « quelque
            // chose joue », la seconde « c'est un flux relançable ». Un
            // contenu déplaçable est exactement ce qui joue sans être un flux.
            seekable: self.lecture && !self.standby && !self.expecting_stream,
            morceau: {
                let mut m = self.metadonnees.etat();
                // Précédence : la durée mesurée par mpv l'emporte sur celle
                // qu'un plugin annonce. `origin` continue de désigner qui a
                // fourni le **morceau** (artiste, titre, album) et non qui a
                // fourni la durée — imprécision assumée plutôt qu'un second
                // champ d'origine pour une seule valeur numérique.
                if self.lecture && !self.standby && self.duree_mesuree_s.is_some() {
                    m.duration_s = self.duree_mesuree_s;
                }
                m
            },
```

`oublie_position` n'est donc appelée que depuis `rafraichit_position` : elle sert à ne pas garder de valeur périmée en mémoire, la garde ci-dessus se chargeant de ce qui sort.

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core"`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-core/src/core.rs
git commit -m "feat(core): la position et la duree mesuree entrent dans l etat publie"
```

---

## Tâche 4 : le tick d'une seconde

**Files:**
- Modify: `crates/ritornello-core/src/main.rs` (boucle `select!`, vers la ligne 405)
- Modify: `crates/ritornello-core/src/core.rs`

**Interfaces:**
- Consomme : `Core::rafraichit_position` (tâche 3).
- Produit : `pub fn tick_position(&self) -> bool` — vrai quand le cœur veut être rappelé dans une seconde.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `crates/ritornello-core/src/core.rs`, module `tests` :

```rust
    #[tokio::test]
    async fn le_tick_ne_s_arme_pas_quand_rien_ne_joue() {
        let (mut core, _, _, _, _dir) = setup();
        assert!(!core.tick_position(), "rien ne joue : rien à rafraîchir");
        // `radio` est la source active de `setup()` : `PlayPause` la fait jouer.
        // Le tick ne s'intéresse pas à la nature du contenu, seulement au fait
        // que quelque chose joue.
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(core.tick_position(), "quelque chose joue : on suit sa position");
        core.handle_command(Command::Stop).await.unwrap();
        assert!(!core.tick_position());
    }

    #[tokio::test]
    async fn le_tick_ne_s_arme_pas_en_veille() {
        let (mut core, _, _, _, _dir) = setup();
        core.handle_command(Command::PlayPause).await.unwrap();
        assert!(core.tick_position());
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.tick_position(), "l'appareil dort");
    }

    /// La règle qui protège les messages éphémères : le tick republie l'état
    /// **avec** l'incrustation en cours, intacte, et sans toucher à son
    /// échéance. C'est l'afficheur qui décide de la mettre par-dessus ou à
    /// côté ; le cœur reste seul maître du moment où elle disparaît.
    #[tokio::test]
    async fn un_rafraichissement_de_position_laisse_l_incrustation_intacte() {
        let (mut core, _, _, _, _dir) = setup();
        // Un contenu **fini** : c'est le seul cas où mpv fournit une position,
        // donc le seul où le rafraîchissement a quelque chose à publier.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap();
        let echeance_avant = core.overlay_deadline();
        assert!(core.etat_lecteur().overlay.is_some(), "l'incrustation volume est là");
        core.regle_progression(Some(30.0), Some(254.0));
        core.rafraichit_position().await;
        assert!(core.etat_lecteur().overlay.is_some(), "et elle y reste");
        assert_eq!(core.overlay_deadline(), echeance_avant, "son échéance n'a pas bougé");
        assert_eq!(core.etat_lecteur().position_s, Some(30));
    }
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core tick"`
Expected: FAIL — `no method named 'tick_position'`.

- [ ] **Étape 3 : implémenter**

Dans `core.rs`, près de `overlay_deadline` :

```rust
    /// Le cœur veut-il être rappelé dans une seconde pour rafraîchir la
    /// position ?
    ///
    /// Armé seulement pendant la lecture : un appareil à l'arrêt ou en veille
    /// ne doit pas produire une trame par seconde pour rien, et la
    /// déduplication de `publie_etat` reprend alors tous ses droits.
    pub fn tick_position(&self) -> bool {
        !self.standby && self.lecture
    }
```

Dans `main.rs`, à côté des deux échéances déjà lues avant le `select!` :

Déclarer, **avant** la boucle et à côté de `let mut retry_at` :

```rust
    /// Échéance du prochain rafraîchissement de position. Absolue, comme
    /// `retry_at` : voir la raison au point d'armement, dans la boucle.
    let mut prochain_tick: Option<tokio::time::Instant> = None;
```

puis, dans la boucle, à côté des deux autres échéances :

```rust
        // Tick de position : une seconde, armé seulement pendant la lecture
        // (voir `Core::tick_position`).
        //
        // L'échéance est **absolue**, comme `retry_at` et `overlay_at`, et
        // c'est un défaut trouvé en relecture qui l'impose. Les trois futurs
        // d'attente sont recréés à chaque tour de boucle, donc chaque fois
        // qu'un bras quelconque se résout — une commande, un événement mpv,
        // un enrichissement, un changement de réglage. Recréer un
        // `sleep_until(at)` sur la même échéance ne change rien ; recréer un
        // `sleep(1 s)` relatif relance le compte à rebours depuis zéro. Le
        // tick n'aurait donc pas lieu une fois par seconde mais une seconde
        // après le dernier réveil du `select!`, et sur un appareil où les
        // événements se succèdent plus vite que cela, il serait repoussé
        // indéfiniment — la position ne bougerait jamais, précisément quand
        // il se passe quelque chose.
        if !core.tick_position() {
            prochain_tick = None;
        } else if prochain_tick.is_none() {
            prochain_tick = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
        }
        // Copie locale (`Instant` est `Copy`) : le futur ci-dessous n'emprunte
        // donc ni `core` ni la variable réassignée dans le bras.
        let position_at = prochain_tick;
        let position_sleep = async {
            match position_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
```

et le bras correspondant dans le `select!` :

```rust
            _ = position_sleep => {
                // Réarmer d'abord, depuis maintenant : la cadence reste d'une
                // seconde quoi qu'il arrive sur les autres bras.
                prochain_tick =
                    Some(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
                // Rafraîchir puis publier : la position ayant changé, la trame
                // franchit la déduplication et part vers la SPA comme vers les
                // afficheurs. L'incrustation éventuellement en cours voyage
                // dans cette même trame, intacte — c'est l'afficheur qui
                // décide de sa place, et le cœur garde la main sur son
                // échéance (bras `overlay_sleep`).
                core.rafraichit_position().await;
                core.publie_etat();
            }
```

`publie_etat` est privée au module `core` : la passer en `pub(crate)` (une seule ligne, `fn publie_etat` → `pub(crate) fn publie_etat`). Pas de méthode-façade autour d'elle : un enrobage qui ne fait qu'appeler l'autre serait du bruit, et la boucle `select!` appelle déjà `overlay_deadline` du même objet.

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core && cargo clippy --workspace --all-targets -- -D warnings"`
Expected: PASS, sans avertissement clippy.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-core/src/core.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): un tick d une seconde publie la position pendant la lecture"
```

---

## Tâche 5 : l'ancre des plugins metadata

**Files:**
- Modify: `crates/ritornello-core/src/metadata.rs`
- Modify: `crates/ritornello-core/src/core.rs`

**Interfaces:**
- Consomme : `Enrichment.position_s` (tâche 1), `Core::rafraichit_position` (tâche 3).
- Produit : `Metadonnees::position_s(&self) -> Option<u32>` ; champ `Core.ancre_position: Option<(u32, Instant)>`.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `crates/ritornello-core/src/metadata.rs`, module `tests` :

```rust
    /// La position suit le **gagnant** de l'arbitrage, comme le reste du
    /// morceau : un plugin moins prioritaire retenu en réserve ne doit pas
    /// imposer sa propre horloge.
    #[test]
    fn la_position_est_celle_du_gagnant() {
        let mut m = Metadonnees::new(vec!["radiofrance".into(), "ouifm".into()]);
        m.set_identity(Some(json!({"url": "https://fip"})));
        m.ajoute(
            "ouifm",
            Enrichment {
                identity: json!({"url": "https://fip"}),
                title: Some("depuis ouifm".into()),
                position_s: Some(200),
                ..Default::default()
            },
        );
        assert_eq!(m.position_s(), Some(200));
        m.ajoute(
            "radiofrance",
            Enrichment {
                identity: json!({"url": "https://fip"}),
                title: Some("depuis radiofrance".into()),
                position_s: Some(12),
                ..Default::default()
            },
        );
        assert_eq!(m.position_s(), Some(12), "le plus prioritaire l'emporte");
    }

    #[test]
    fn sans_enrichissement_il_n_y_a_pas_de_position() {
        let m = Metadonnees::new(vec!["radiofrance".into()]);
        assert_eq!(m.position_s(), None);
    }
```

Dans `crates/ritornello-core/src/core.rs`, module `tests` :

```rust
    /// Entre deux interrogations du direct — plusieurs dizaines de secondes
    /// chez Radio France — c'est le cœur qui fait avancer la barre, depuis
    /// l'ancre posée à la réception.
    #[tokio::test]
    async fn l_ancre_d_un_enrichissement_avance_toute_seule() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Un **flux** : c'est le seul contexte où l'ancre parle (sur un
        // contenu fini, mpv a la parole). `radio` est déjà la source active.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(254),
                position_s: Some(87),
                ..Default::default()
            },
        );
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(87));
        core.avance_ancre_pour_test(std::time::Duration::from_secs(3));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(90));
    }

    /// Un morceau qui finit avant que la station ne l'annonce ne doit pas
    /// afficher « 4:31 / 4:14 ».
    #[tokio::test]
    async fn la_position_annoncee_est_plafonnee_par_la_duree() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let id = serde_json::json!({"url": "http://fip"});
        core.handle_source_update("radio", joue(id.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment {
                identity: id,
                title: Some("Bikwix".into()),
                duration_s: Some(100),
                position_s: Some(98),
                ..Default::default()
            },
        );
        core.avance_ancre_pour_test(std::time::Duration::from_secs(30));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(100));
    }

    /// L'ancre du morceau précédent ne doit pas continuer d'avancer sous le
    /// titre du suivant.
    #[tokio::test]
    async fn un_changement_d_identite_efface_l_ancre() {
        let (mut core, _np_rx, _etat_rx, _dir) = setup_metadonnees(vec!["radiofrance".into()]);
        // Flux : `radio` est déjà la source active de ce montage.
        core.handle_command(Command::PlayPause).await.unwrap();
        let un = serde_json::json!({"url": "un"});
        core.handle_source_update("radio", joue(un.clone()));
        core.handle_enrichment(
            "radiofrance",
            Enrichment { identity: un, title: Some("A".into()), position_s: Some(50), ..Default::default() },
        );
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, Some(50));
        core.handle_source_update("radio", joue(serde_json::json!({"url": "deux"})));
        core.rafraichit_position().await;
        assert_eq!(core.etat_lecteur().position_s, None);
    }
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core ancre"`
Expected: FAIL — `no method named 'position_s'` / `no method named 'avance_ancre_pour_test'`.

- [ ] **Étape 3 : implémenter**

Dans `metadata.rs`, à côté de `etat()` :

```rust
    /// Position déclarée par le **gagnant** de l'arbitrage, s'il en déclare
    /// une.
    ///
    /// Sortie à part de `etat()` plutôt que glissée dans `Morceau` : `Morceau`
    /// décrit ce qui est affichable d'un morceau, valeurs stables tant qu'il
    /// joue, alors qu'une position ne vaut que pour l'instant où elle a été
    /// dite. Ce module n'a d'ailleurs aucune horloge, et c'est délibéré (voir
    /// l'en-tête) : c'est au cœur d'ancrer cette valeur et de l'avancer.
    pub fn position_s(&self) -> Option<u32> {
        for plugin in &self.ordre {
            if let Some(e) = self.enrichissements.get(plugin) {
                return e.position_s;
            }
        }
        None
    }
```

Dans `core.rs`, ajouter le champ après `duree_mesuree_s` :

```rust
    /// Position annoncée par un plugin `metadata`, et l'instant où elle est
    /// arrivée. Le cœur l'avance lui-même entre deux annonces — Radio France
    /// n'interroge le direct que toutes les quelques dizaines de secondes, et
    /// sans cette avance la barre resterait figée entre deux réponses.
    ancre_position: Option<(u32, Instant)>,
```

initialisé à `None` dans `Core::new`, effacé dans `oublie_position` :

```rust
    fn oublie_position(&mut self) {
        self.position_s = None;
        self.duree_mesuree_s = None;
        self.ancre_position = None;
    }
```

Dans `handle_enrichment`, juste avant `self.publie_etat();` :

```rust
        // Poser l'ancre à la réception : c'est le seul instant où l'écoulé
        // annoncé est exact.
        //
        // **Seulement quand c'est le gagnant qui vient de parler**, et c'est un
        // défaut trouvé en relecture. Un plugin retenu en réserve peut répondre
        // à tout moment (un titre corrigé, une pochette) sans rien apprendre de
        // neuf sur l'avancement : réancrer alors relirait la position
        // **inchangée** du gagnant en la datant de maintenant, et la barre
        // reculerait d'un coup de tout ce qu'elle avait avancé. Le `match`
        // ci-dessus distingue déjà les deux cas pour le journal.
        //
        // Un gagnant qui réémet à l'identique n'arrive jamais ici : `ajoute`
        // déduplique et rend `false`. Et un plugin plus prioritaire qui répond
        // pour la première fois **devient** le gagnant, donc son annonce ancre
        // bien, ce qui est voulu.
        if self.metadonnees.gagnant() == Some(plugin) {
            self.ancre_position = self.metadonnees.position_s().map(|p| (p, Instant::now()));
        }
```

Dans `set_identity`, dans la branche qui suit `if !self.metadonnees.set_identity(identity) { return; }` (donc quand l'identité a réellement changé), ajouter avant l'envoi de `NowPlaying` :

```rust
        // Le morceau a changé : l'ancre du précédent ne doit pas continuer
        // d'avancer sous le titre du suivant.
        self.ancre_position = None;
```

Dans `rafraichit_position`, remplacer la branche `if self.expecting_stream` par :

```rust
        if self.expecting_stream {
            // Flux : le `time-pos` de mpv compte depuis le début de la
            // connexion, sans rapport avec le morceau. La position vient donc
            // d'un plugin `metadata`, ancrée à sa réception et avancée ici.
            self.duree_mesuree_s = None;
            self.position_s = self.ancre_position.map(|(depart, pose)| {
                let ecoule = pose.elapsed().as_secs();
                let brute = depart.saturating_add(u32::try_from(ecoule).unwrap_or(u32::MAX));
                // Plafonnée par la durée annoncée : un morceau qui finit avant
                // que la station ne l'annonce ne doit pas afficher
                // « 4:31 / 4:14 ».
                match self.metadonnees.etat().duration_s {
                    Some(duree) => brute.min(duree),
                    None => brute,
                }
            });
            return;
        }
```

Ajouter l'assistant de test **dans le bloc `impl Core<FakePlayer>` déjà créé à la tâche 3**, à côté de `regle_progression` — un seul bloc pour les assistants du cœur factice, pas deux :

```rust
        /// Recule l'ancre de `duree` : le test avance le temps sans dormir.
        fn avance_ancre_pour_test(&mut self, duree: std::time::Duration) {
            if let Some((p, pose)) = self.ancre_position {
                self.ancre_position = Some((p, pose - duree));
            }
        }
```

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core"`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-core/src/metadata.rs crates/ritornello-core/src/core.rs
git commit -m "feat(core): la position annoncee par un plugin est ancree puis avancee"
```

---

## Tâche 6 : les trois commandes de déplacement et leur pas réglable

**Files:**
- Modify: `crates/ritornello-core/src/state.rs`
- Modify: `crates/ritornello-core/src/status.rs`
- Modify: `crates/ritornello-core/src/core.rs`
- Modify: `crates/ritornello-core/src/locales/en.toml`
- Modify: `deploy/locales/core/fr.toml`

**Interfaces:**
- Consomme : `Command::SeekForward/SeekBackward/SeekTo` (tâche 1), `Player::seek_relative/seek_absolute` (tâche 2), `PlayerState.seekable` (tâche 3).
- Produit : `Settings.seek_step_s: u32` (défaut 10, bornes 1..=120) ; clé i18n `settings_seek_step_out_of_range`.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `crates/ritornello-core/src/state.rs`, module `tests`, compléter `settings_par_defaut` avec :

```rust
        assert_eq!(s.seek_step_s, 10);
```

Dans `crates/ritornello-core/src/status.rs`, module `tests` :

```rust
    #[tokio::test]
    async fn le_pas_de_deplacement_hors_bornes_est_refuse() {
        for (pas, valide) in [(0u32, false), (1, true), (10, true), (120, true), (121, false)] {
            let s = crate::state::Settings { seek_step_s: pas, ..Default::default() };
            assert_eq!(validate_settings(&s).is_ok(), valide, "pas = {pas}");
        }
    }

    /// Le refus est une phrase du catalogue, jamais une chaîne en dur, et il
    /// **cite ses bornes** : c'est la règle « les bornes ne peuvent pas
    /// mentir » que le chantier i18n a posée.
    #[test]
    fn le_refus_du_pas_cite_ses_bornes() {
        // Chemin inexistant : le catalogue retombe sur l'anglais embarqué,
        // celui-là même que la clé doit désormais contenir.
        let catalogue = ritornello_i18n::Catalog::load(
            "core",
            "en",
            std::path::Path::new("/inexistant"),
            crate::core::EN,
        );
        let message = SettingsError::SeekStep { min: 1, max: 120 }.message(&catalogue);
        assert!(message.contains('1') && message.contains("120"), "{message}");
        assert!(!message.contains("{min}"), "clé non substituée : {message}");
        assert_ne!(message, "settings_seek_step_out_of_range", "clé absente du catalogue");
    }
```

Dans `crates/ritornello-core/src/core.rs`, module `tests` :

```rust
    #[tokio::test]
    async fn les_touches_de_deplacement_agissent_sur_un_contenu_fini() {
        let (mut core, calls, _, _, _dir) = setup();
        // Contenu fini : bascule de `radio` (source active par défaut) vers `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekBackward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        let journal = calls.lock().unwrap().clone();
        assert!(journal.contains(&"seek_relative 10".to_string()), "{journal:?}");
        assert!(journal.contains(&"seek_relative -10".to_string()), "{journal:?}");
        assert!(journal.contains(&"seek_absolute 198".to_string()), "{journal:?}");
    }

    /// Sur un direct, la touche ne fait rien — comme une touche non liée. Pas
    /// de message, pas de trame : le contenu n'est pas parcourable, et le dire
    /// n'apprendrait rien à qui vient d'appuyer.
    #[tokio::test]
    async fn les_touches_de_deplacement_sont_ignorees_sur_un_flux() {
        let (mut core, calls, _, _, _dir) = setup();
        // Flux : `radio` est déjà la source active, `PlayPause` la fait jouer.
        core.handle_command(Command::PlayPause).await.unwrap();
        calls.lock().unwrap().clear();
        core.handle_command(Command::SeekForward).await.unwrap();
        core.handle_command(Command::SeekTo(198)).await.unwrap();
        assert!(
            calls.lock().unwrap().iter().all(|c| !c.starts_with("seek_")),
            "{:?}",
            calls.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn le_pas_de_deplacement_suit_le_reglage() {
        let (mut core, calls, _, _, _dir) = setup();
        // `set_settings` existe déjà (elle sert la route `PUT /api/settings`).
        core.set_settings(crate::state::Settings { seek_step_s: 30, ..Default::default() });
        // Contenu fini : bascule de `radio` vers `cd`.
        core.handle_command(Command::SourceCycle).await.unwrap();
        core.handle_command(Command::SeekForward).await.unwrap();
        assert!(calls.lock().unwrap().contains(&"seek_relative 30".to_string()));
    }
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core deplacement"`
Expected: FAIL — `no field 'seek_step_s'`, `no variant 'SeekStep'`.

- [ ] **Étape 3 : implémenter**

Dans `state.rs`, dans `struct Settings`, après `tens_window_ms` :

```rust
    /// Pas des touches « avancer » / « reculer », en secondes.
    ///
    /// Réglable là où le pas de volume est figé, parce que la bonne valeur
    /// dépend de ce qu'on écoute : dix secondes pour rattraper une phrase,
    /// une minute pour traverser un mouvement.
    pub seek_step_s: u32,
```

et `seek_step_s: 10,` dans `Default`.

Dans `status.rs` : ajouter la constante à côté des autres bornes,

```rust
/// Bornes du pas de déplacement, en secondes. Une seconde en bas parce qu'un
/// pas nul ne déplace rien ; deux minutes en haut parce qu'au-delà, la touche
/// ne sert plus à se déplacer dans une piste mais à en changer.
const SEEK_STEP_S: std::ops::RangeInclusive<u32> = 1..=120;
```

la variante `SeekStep { min: u32, max: u32 }` dans `enum SettingsError`, son bras dans `message` :

```rust
            SettingsError::SeekStep { min, max } => catalog
                .get("settings_seek_step_out_of_range")
                .replace("{min}", &min.to_string())
                .replace("{max}", &max.to_string()),
```

son bras dans `Display` :

```rust
            SettingsError::SeekStep { min, max } => {
                write!(f, "seek step out of range ({min}-{max} s)")
            }
```

et la vérification dans `validate_settings`, avant le `Ok(())` :

```rust
    if !SEEK_STEP_S.contains(&s.seek_step_s) {
        return Err(SettingsError::SeekStep {
            min: *SEEK_STEP_S.start(),
            max: *SEEK_STEP_S.end(),
        });
    }
```

Dans `core.rs`, dans `appliquer_commande`, après la branche `Command::Mute` :

```rust
            Command::SeekForward | Command::SeekBackward => {
                // Ignorée en silence sur un contenu non parcourable : la
                // touche se comporte comme une touche non liée, ce que la
                // télécommande sait déjà faire. Un message n'apprendrait rien
                // à qui vient d'appuyer.
                if self.lecture && !self.expecting_stream {
                    let pas = i64::from(self.settings.seek_step_s);
                    let delta = if cmd == Command::SeekForward { pas } else { -pas };
                    self.player.seek_relative(delta).await?;
                    self.rafraichit_position().await;
                }
            }
            Command::SeekTo(position_s) => {
                if self.lecture && !self.expecting_stream {
                    self.player.seek_absolute(position_s).await?;
                    self.rafraichit_position().await;
                }
            }
```

Dans `crates/ritornello-core/src/locales/en.toml`, à côté des autres refus de réglage :

```toml
settings_seek_step_out_of_range = "seek step out of range ({min}-{max} s)"
```

et les libellés d'IHM, à côté de `remote_*` et des libellés de réglages :

```toml
remote_seek_back = "Rewind"
remote_seek_forward = "Fast forward"
seek_step_label = "Seek step (s)"
seek_card_title = "Seeking"
position_label = "Position"
```

Dans `deploy/locales/core/fr.toml`, les mêmes clés :

```toml
settings_seek_step_out_of_range = "pas de déplacement hors bornes ({min}-{max} s)"
remote_seek_back = "Reculer"
remote_seek_forward = "Avancer"
seek_step_label = "Pas de déplacement (s)"
seek_card_title = "Déplacement"
position_label = "Position"
```

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings"`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-core/src deploy/locales/core/fr.toml
git commit -m "feat(core): trois commandes de deplacement, au pas reglable"
```

---

## Tâche 7 : Radio France annonce où en est le morceau

**Files:**
- Modify: `crates/ritornello-plugin-radiofrance-metas/src/live.rs`
- Modify: `crates/ritornello-plugin-radiofrance-metas/src/main.rs`

**Interfaces:**
- Consomme : `Enrichment.position_s` (tâche 1).
- Produit : `live::Meta.start_time: Option<u64>` (époque Unix, secondes).

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `live.rs`, module `tests` :

```rust
    /// `startTime` est retenu **brut** : c'est au moment d'émettre
    /// l'enrichissement qu'on en déduit l'écoulé, pas au moment d'analyser la
    /// réponse — l'analyse reste pure, sans horloge, comme tout ce module.
    #[test]
    fn le_direct_retient_le_debut_du_morceau() {
        let m = parse_direct(REPONSE_FIP).unwrap().meta.unwrap();
        assert_eq!(m.start_time, Some(1786722565));
        assert_eq!(m.duration_s, Some(197));
    }

    /// Même filtre que la durée : sans `firstLineSongUuid`, les bornes sont
    /// celles d'une tranche d'antenne et non d'un morceau. En déduire une
    /// position afficherait une progression fausse — mesuré à une heure sur
    /// Mouv'.
    #[test]
    fn une_tranche_d_antenne_ne_donne_pas_de_debut_de_morceau() {
        let m = parse_direct(REPONSE_MOUV).unwrap().meta.unwrap();
        assert_eq!(m.start_time, None);
        assert_eq!(m.duration_s, None);
    }
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-plugin-radiofrance-metas"`
Expected: FAIL — `no field 'start_time' on type 'Meta'`.

- [ ] **Étape 3 : implémenter**

Dans `live.rs`, ajouter à `struct Meta` :

```rust
    /// Début du morceau, en secondes depuis l'époque Unix, tel que le direct
    /// l'annonce. Brut : c'est l'émission de l'enrichissement qui en déduit
    /// l'écoulé, pour que ce module reste sans horloge et testable sur des
    /// captures.
    pub start_time: Option<u64>,
```

Dans `parse_direct`, dans la construction de `Meta`, ajouter :

```rust
        // Même filtre que la durée : sans `firstLineSongUuid`, les bornes sont
        // celles d'une tranche d'antenne, pas d'un morceau.
        start_time: now
            .get("startTime")
            .and_then(Value::as_u64)
            .filter(|_| est_un_morceau)
            .filter(|_| duree.is_some_and(|d| d <= DUREE_MAX_S)),
```

Dans `main.rs`, dans `next_enrichment`, remplacer la construction de l'`Enrichment` par :

```rust
                return Enrichment {
                    identity: identite.clone(),
                    artist: meta.artist,
                    title: meta.title,
                    // Absent le plus souvent : le direct n'en donne pas, il se
                    // lit dans la grille, qui a fréquemment un morceau de
                    // retard (voir `live::album_dans_grille`).
                    album: meta.album,
                    duration_s: meta.duration_s,
                    // L'écoulé est calculé **ici**, au moment d'émettre : c'est
                    // le seul instant où il est exact, et le cœur l'ancre à sa
                    // réception. Une horloge décalée ou un `startTime` dans le
                    // futur donnerait un écoulé négatif : `checked_sub` le
                    // ramène à « je ne sais pas » plutôt qu'à zéro, qui
                    // prétendrait savoir.
                    position_s: meta.start_time.and_then(|debut| {
                        let maintenant = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()?
                            .as_secs();
                        maintenant.checked_sub(debut).and_then(|e| u32::try_from(e).ok())
                    }),
                };
```

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-plugin-radiofrance-metas"`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-plugin-radiofrance-metas/src
git commit -m "feat(plugin-radiofrance-metas): annoncer ou en est le morceau diffuse"
```

---

## Tâche 8 : la SPA reçoit les deux champs

**Files:**
- Modify: `web/app/src/types.ts`
- Modify: `web/app/src/composables/usePlayer.ts`
- Test: `web/app/src/composables/usePlayer.test.ts`

**Interfaces:**
- Consomme : la charge utile de la tâche 1.
- Produit : `PlayerPayload.position_s: number | null`, `PlayerPayload.seekable: boolean`, `SettingsPayload.seek_step_s: number`, `formatePosition(secondes): string | null`.

- [ ] **Étape 1 : écrire le test qui échoue**

Dans `web/app/src/composables/usePlayer.test.ts` :

```ts
import { describe, expect, it } from 'vitest'
import { formatePosition } from './usePlayer'

describe('formatePosition', () => {
  // `formateDuree` refuse les valeurs <= 0, ce qui est juste pour une duree
  // et faux pour une position : `0:00` est un instant parfaitement legitime.
  // Deux fonctions plutot qu'un assouplissement de la premiere, qui ferait
  // reapparaitre des « 0:00 » la ou le refus servait.
  it('accepte zero', () => {
    expect(formatePosition(0)).toBe('0:00')
  })
  it('formate minutes et secondes', () => {
    expect(formatePosition(87)).toBe('1:27')
    expect(formatePosition(3725)).toBe('62:05')
  })
  it('rend null sur une absence', () => {
    expect(formatePosition(null)).toBeNull()
    expect(formatePosition(undefined)).toBeNull()
    expect(formatePosition(-1)).toBeNull()
  })
})
```

- [ ] **Étape 2 : voir le test échouer**

Run: `npm test -w app -- usePlayer`
Expected: FAIL — `formatePosition is not a function`.

- [ ] **Étape 3 : implémenter**

Dans `web/app/src/composables/usePlayer.ts`, à côté de `formateDuree` :

```ts
/**
 * Meme forme que `formateDuree`, mais `0` est une valeur legitime : une
 * position au tout debut d'une piste s'ecrit « 0:00 ». Une fonction distincte
 * plutot qu'un assouplissement de l'autre, dont le refus des valeurs nulles
 * evite d'afficher « 0:00 » comme duree d'un morceau dont on ignore la duree.
 */
export function formatePosition(secondes: number | null | undefined): string | null {
  if (typeof secondes !== 'number' || !Number.isFinite(secondes) || secondes < 0) return null
  const m = Math.floor(secondes / 60)
  const s = Math.floor(secondes % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}
```

Dans `web/app/src/types.ts`, dans `PlayerPayload`, après `duration_s` :

```ts
  /**
   * Ou en est ce qui joue, en secondes, a l'instant ou la trame a ete
   * publiee — le coeur en pousse une par seconde pendant la lecture.
   * `null` quand personne ne sait : rien ne joue, ou c'est un flux qu'aucun
   * plugin `metadata` ne suit.
   */
  position_s: number | null
  /**
   * Ce qui joue accepte un deplacement. Distinct de « une duree est connue » :
   * Radio France annonce la duree d'un morceau sur un direct qu'on ne peut pas
   * rembobiner. C'est ce champ, et lui seul, qui rend la barre cliquable.
   */
  seekable: boolean
```

et dans `SettingsPayload`, après `tens_window_ms` :

```ts
  /** Pas des touches « avancer » / « reculer », en secondes. */
  seek_step_s: number
```

- [ ] **Étape 4 : voir le test passer**

Run: `npm test -w app -- usePlayer && npm run typecheck`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add web/app/src/types.ts web/app/src/composables/usePlayer.ts web/app/src/composables/usePlayer.test.ts
git commit -m "feat(web): la charge utile du lecteur porte position et deplacabilite"
```

---

## Tâche 9 : la barre de progression

**Files:**
- Create: `web/app/src/components/BarreProgression.vue`
- Create: `web/app/src/components/BarreProgression.test.ts`
- Modify: `web/app/src/components/PlayerCard.vue`
- Modify: `web/app/src/components/PlayerCard.test.ts`

**Interfaces:**
- Consomme : `formatePosition` et les champs de la tâche 8.
- Produit : composant `BarreProgression` — props `{ position: number | null; duree: number | null; deplacable: boolean; pas: number }`, événement `deplacer(secondes: number)`.

- [ ] **Étape 1 : écrire les tests qui échouent**

`web/app/src/components/BarreProgression.test.ts` :

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import BarreProgression from './BarreProgression.vue'

const monte = (props: Record<string, unknown>) =>
  mount(BarreProgression, { props: { position: 87, duree: 254, deplacable: false, pas: 10, ...props } })

describe('BarreProgression', () => {
  it('affiche la position et la duree', () => {
    const w = monte({})
    expect(w.get('[data-position]').text()).toBe('1:27')
    expect(w.get('[data-duree-totale]').text()).toBe('4:14')
  })

  // Une barre sans fin n'apprend rien : sans duree, seul l'ecoule s'affiche.
  it('sans duree, pas de barre', () => {
    const w = monte({ duree: null })
    expect(w.find('[data-barre]').exists()).toBe(false)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('remplit la barre au prorata', () => {
    const w = monte({})
    expect(w.get('[data-remplissage]').attributes('style')).toContain('34')
  })

  // C'est `deplacable` qui decide, pas la presence d'une duree : Radio France
  // annonce une duree sur un direct qu'on ne peut pas rembobiner.
  it('inerte quand le contenu n est pas deplacable', async () => {
    const w = monte({ deplacable: false })
    await w.get('[data-barre]').trigger('click')
    expect(w.emitted('deplacer')).toBeUndefined()
    expect(w.get('[data-barre]').attributes('role')).toBeUndefined()
  })

  it('emet la seconde visee au clic', async () => {
    const w = monte({ deplacable: true })
    const barre = w.get('[data-barre]')
    barre.element.getBoundingClientRect = () =>
      ({ left: 0, width: 200, top: 0, height: 4, right: 200, bottom: 4, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect
    await barre.trigger('click', { clientX: 100 })
    expect(w.emitted('deplacer')?.[0]).toEqual([127])
  })

  // Sans le clavier, la barre serait la seule commande de la page hors
  // d'atteinte sans souris, sur une page dont toutes les autres sont des
  // boutons.
  it('se pilote au clavier', async () => {
    const w = monte({ deplacable: true })
    const barre = w.get('[data-barre]')
    expect(barre.attributes('role')).toBe('slider')
    expect(barre.attributes('tabindex')).toBe('0')
    await barre.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('deplacer')?.[0]).toEqual([97])
    await barre.trigger('keydown', { key: 'ArrowLeft' })
    expect(w.emitted('deplacer')?.[1]).toEqual([77])
    await barre.trigger('keydown', { key: 'Home' })
    expect(w.emitted('deplacer')?.[2]).toEqual([0])
    await barre.trigger('keydown', { key: 'End' })
    expect(w.emitted('deplacer')?.[3]).toEqual([254])
  })
})
```

Dans `web/app/src/components/PlayerCard.test.ts`, il existe déjà un assistant `complet(partiel: Partial<PlayerPayload>): PlayerPayload` qui remplit les champs manquants. **Y ajouter `position_s: null` et `seekable: false`** parmi ses valeurs par défaut (sans quoi le typage casse), puis ajouter les deux cas :

```ts
  it('montre la barre quand une position est connue', () => {
    const w = mount(PlayerCard, {
      props: {
        etat: complet({ title: 'Bikwix', position_s: 87, duration_s: 254, seekable: true }),
        pasDeplacement: 10,
      },
    })
    expect(w.find('[data-barre]').exists()).toBe(true)
    expect(w.get('[data-position]').text()).toBe('1:27')
  })

  it('ne montre rien de la progression quand aucune position n est connue', () => {
    const w = mount(PlayerCard, {
      props: {
        etat: complet({ title: 'Bikwix', position_s: null, duration_s: 254 }),
        pasDeplacement: 10,
      },
    })
    expect(w.find('[data-position]').exists()).toBe(false)
  })
```

`pasDeplacement` devient une prop obligatoire de `PlayerCard` : les montages déjà présents dans ce fichier doivent la recevoir eux aussi.

- [ ] **Étape 2 : voir les tests échouer**

Run: `npm test -w app -- BarreProgression PlayerCard`
Expected: FAIL — `Cannot find module './BarreProgression.vue'`.

- [ ] **Étape 3 : implémenter**

`web/app/src/components/BarreProgression.vue` :

```vue
<script setup lang="ts">
import { computed } from 'vue'
import { formatePosition } from '../composables/usePlayer'

// Composant local a la SPA plutot qu'element du kit : seule la carte Player
// s'en sert, et le kit est le contrat des pages de plugins.
const props = defineProps<{
  position: number | null
  duree: number | null
  /** Le contenu accepte un deplacement (`seekable` de la charge utile). */
  deplacable: boolean
  /** Pas du clavier, en secondes : le meme que celui des touches physiques. */
  pas: number
}>()
const emit = defineEmits<{ deplacer: [secondes: number] }>()

const texteEcoule = computed(() => formatePosition(props.position))
const texteDuree = computed(() => formatePosition(props.duree))
// Une barre sans fin n'apprend rien : sans duree connue, seul l'ecoule
// s'affiche.
const barreVisible = computed(() => props.duree != null && props.duree > 0)
const pourcent = computed(() => {
  if (!barreVisible.value || props.position == null) return 0
  return Math.min(100, Math.max(0, (props.position / (props.duree as number)) * 100))
})

function viser(e: MouseEvent): void {
  if (!props.deplacable || !barreVisible.value) return
  const rect = (e.currentTarget as HTMLElement).getBoundingClientRect()
  if (rect.width <= 0) return
  const ratio = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width))
  emit('deplacer', Math.round(ratio * (props.duree as number)))
}

function auClavier(e: KeyboardEvent): void {
  if (!props.deplacable || props.duree == null) return
  const depuis = props.position ?? 0
  const cible = {
    ArrowRight: depuis + props.pas,
    ArrowUp: depuis + props.pas,
    ArrowLeft: depuis - props.pas,
    ArrowDown: depuis - props.pas,
    Home: 0,
    End: props.duree,
  }[e.key]
  if (cible === undefined) return
  e.preventDefault()
  emit('deplacer', Math.min(props.duree, Math.max(0, cible)))
}
</script>

<template>
  <div v-if="texteEcoule" class="mt-2 space-y-1" data-progression>
    <div
      v-if="barreVisible"
      class="h-1.5 w-full rounded-full bg-muted"
      :class="deplacable ? 'cursor-pointer' : ''"
      data-barre
      :role="deplacable ? 'slider' : undefined"
      :tabindex="deplacable ? 0 : undefined"
      :aria-valuemin="deplacable ? 0 : undefined"
      :aria-valuemax="deplacable ? duree ?? undefined : undefined"
      :aria-valuenow="deplacable ? position ?? undefined : undefined"
      :aria-valuetext="deplacable ? texteEcoule : undefined"
      @click="viser"
      @keydown="auClavier"
    >
      <div class="h-full rounded-full bg-primary" :style="{ width: pourcent + '%' }" data-remplissage />
    </div>
    <div class="flex justify-between text-xs text-muted-foreground">
      <span data-position>{{ texteEcoule }}</span>
      <span v-if="texteDuree" data-duree-totale>{{ texteDuree }}</span>
    </div>
  </div>
</template>
```

Dans `PlayerCard.vue` : importer le composant, déclarer l'événement remonté au parent (c'est `HomeView` qui poste les commandes, la carte n'en poste aucune aujourd'hui — vérifier ce point avant d'écrire, et suivre la façon dont la carte est câblée) :

```vue
import BarreProgression from './BarreProgression.vue'
const emit = defineEmits<{ deplacer: [secondes: number] }>()
defineProps<{ etat: PlayerPayload | null; pasDeplacement: number }>()
```

et, dans le bloc « En écoute », juste après la ligne `data-album` :

```vue
        <BarreProgression
          :position="etat?.position_s ?? null"
          :duree="etat?.duration_s ?? null"
          :deplacable="etat?.seekable ?? false"
          :pas="pasDeplacement"
          @deplacer="(s) => emit('deplacer', s)"
        />
```

Dans `HomeView.vue`, la ligne `<PlayerCard :etat="etat" />` devient :

```vue
    <PlayerCard
      :etat="etat"
      :pas-deplacement="reglages.seek_step_s"
      @deplacer="(s: number) => send({ cmd: 'SeekTo', arg: s })"
    />
```

`reglages` (un `ref<SettingsPayload>` alimenté par `/api/settings`) et `send` existent déjà dans ce fichier — ils servent l'auto-répétition du volume. Ajouter `seek_step_s: 10` à l'objet de repli de `reglages`, comme les autres réglages y ont déjà leur valeur de repli.

- [ ] **Étape 4 : voir les tests passer**

Run: `npm test -w app && npm run typecheck`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add web/app/src/components web/app/src/views/HomeView.vue
git commit -m "feat(web): une barre de progression, cliquable quand le contenu s y prete"
```

---

## Tâche 10 : les deux boutons et la carte de réglage

**Files:**
- Modify: `web/app/src/views/remoteCommands.ts`
- Modify: `web/app/src/views/ConfigView.vue`
- Test: `web/app/src/views/HomeView.test.ts`, `web/app/src/views/ConfigView.test.ts`, `web/app/src/i18nKeysUsed.test.ts`

**Interfaces:**
- Consomme : `Command::SeekForward/SeekBackward` (tâche 1), `SettingsPayload.seek_step_s` (tâche 8), clés i18n (tâche 6).
- Produit : deux entrées de plus dans `REMOTE_ROWS`, un champ de plus dans le formulaire de réglages.

- [ ] **Étape 1 : écrire les tests qui échouent**

Dans `web/app/src/views/HomeView.test.ts` :

```ts
  // Dans la rangee, l'ordre suit le sens du geste : reculer avant avancer,
  // comme « precedent » avant « suivant » et « moins » avant « plus ».
  it('offre les deux touches de deplacement, dans le sens du geste', () => {
    const cles = REMOTE_ROWS.flat().map((c) => c.key)
    expect(cles).toContain('remote_seek_back')
    expect(cles).toContain('remote_seek_forward')
    expect(cles.indexOf('remote_seek_back')).toBeLessThan(cles.indexOf('remote_seek_forward'))
  })
```

Le fichier contient déjà `expect(REMOTE_COMMANDS).toHaveLength(10)` et une assertion sur la liste triée des noms de commandes : **porter le compte à 12** et ajouter `'SeekBackward'` et `'SeekForward'` à cette liste.

Dans `web/app/src/views/ConfigView.test.ts`, sur le modèle du test existant du champ `tens_window_ms` :

```ts
  it('envoie le pas de deplacement', async () => {
    // ... meme montage que le test voisin des reglages, avec :
    // reglages.seek_step_s edite puis enregistre
    expect(corpsEnvoye.seek_step_s).toBe(30)
  })
```

- [ ] **Étape 2 : voir les tests échouer**

Run: `npm test -w app -- HomeView ConfigView i18nKeysUsed`
Expected: FAIL — clés absentes, compte de commandes faux.

- [ ] **Étape 3 : implémenter**

Dans `remoteCommands.ts`, insérer une rangée après la rangée transport (`play_pause` / `stop`) :

```ts
  [
    { key: 'remote_seek_back', cmd: { cmd: 'SeekBackward' } },
    { key: 'remote_seek_forward', cmd: { cmd: 'SeekForward' } },
  ],
```

et corriger le commentaire de tête, qui annonce « dix commandes simples ».

Dans `ConfigView.vue` : ajouter `seek_step_s: 10` à l'objet `reglages` initial, `seek_step_s: Number(reglages.value.seek_step_s)` au corps du `PUT`, et le champ dans la carte des réglages, sur le modèle exact du champ `tens_window_ms` :

```vue
            <Label class="grid gap-1 text-sm">
              {{ t('seek_step_label') }}
              <Input type="number" min="1" max="120" class="w-32"
                v-model="reglages.seek_step_s" />
            </Label>
```

- [ ] **Étape 4 : voir les tests passer**

Run: `npm test --workspaces && npm run typecheck`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add web/app/src/views
git commit -m "feat(web): deux touches de deplacement et le reglage de leur pas"
```

---

## Tâche 11 : la télécommande physique apprend les deux touches

**Files:**
- Modify: `crates/ritornello-plugin-generic-input/ui/src/preset-toml.ts`
- Modify: `crates/ritornello-plugin-generic-input/ui/src/preset-toml.test.ts`
- Modify: `crates/ritornello-plugin-generic-input/src/locales/en.toml`
- Modify: `deploy/locales/generic-input/fr.toml`

**Interfaces:**
- Consomme : `Command::SeekForward/SeekBackward` (tâche 1).
- Produit : deux entrées de plus dans `ACTIONS`, clés `act_seek_back` / `act_seek_forward`.

- [ ] **Étape 1 : écrire le test qui échoue**

Dans `preset-toml.test.ts`, compléter la liste attendue des commandes avec `'SeekBackward', 'SeekForward'` et ajouter :

```ts
  it('offre les deux actions de deplacement, apres le transport', () => {
    const cles = ACTIONS.map((a) => a.key)
    expect(cles).toContain('act_seek_back')
    expect(cles).toContain('act_seek_forward')
    expect(cles.indexOf('act_seek_back')).toBeLessThan(cles.indexOf('act_seek_forward'))
  })
```

Le test voisin verrouille le nombre d'actions (« Les 21 actions ») : le porter à 23 et mettre à jour le commentaire du fichier source.

- [ ] **Étape 2 : voir le test échouer**

Run: `npm test -w @ritornello/plugin-generic-input-ui -- preset-toml`

Si ce nom d'espace de travail n'est pas exact, le lire dans le `package.json` du dossier `ui` du plugin et employer celui-là.
Expected: FAIL.

- [ ] **Étape 3 : implémenter**

Dans `preset-toml.ts`, après `{ key: 'act_stop', cmd: { cmd: 'Stop' } },` :

```ts
  { key: 'act_seek_back', cmd: { cmd: 'SeekBackward' } },
  { key: 'act_seek_forward', cmd: { cmd: 'SeekForward' } },
```

Dans `crates/ritornello-plugin-generic-input/src/locales/en.toml` :

```toml
act_seek_back = "Rewind"
act_seek_forward = "Fast forward"
```

Dans `deploy/locales/generic-input/fr.toml` :

```toml
act_seek_back = "Reculer"
act_seek_forward = "Avancer"
```

Les presets livrés (`deploy/input-presets/mce.toml`, `keyboard.toml`) ne reçoivent une liaison par défaut **que si** une touche évidente existe dans leur table (les codes `evdev` `KEY_REWIND` / `KEY_FASTFORWARD`, 168 et 208). Vérifier la présence de ces codes dans les fichiers avant d'ajouter quoi que ce soit ; à défaut, ne rien ajouter — les deux actions restent apprenables, ce que la page sait déjà présenter.

- [ ] **Étape 4 : voir les tests passer**

Run: `npm test --workspaces`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-plugin-generic-input deploy/locales/generic-input/fr.toml deploy/input-presets
git commit -m "feat(plugin-generic-input): deux actions apprenables pour se deplacer"
```

---

## Tâche 12 : l'afficheur console ne bronche pas

**Files:**
- Modify: `crates/ritornello-plugin-console/src/display.rs` (tests seulement)

**Interfaces:**
- Consomme : `PlayerState.position_s` (tâche 1).
- Produit : rien — cette tâche verrouille une décision de conception par un test.

- [ ] **Étape 1 : écrire le test qui échoue**

Dans `display.rs`, module `tests` :

```rust
    /// Décision de conception : cet afficheur **ne montre pas** la position.
    /// Trois lignes d'une vingtaine de colonnes déjà pleines, et une horloge y
    /// coûterait un effacement d'écran par seconde — or le cœur en publie une
    /// trame par seconde pendant toute la lecture. Le champ voyage quand même
    /// jusqu'ici : tout autre plugin d'affichage peut s'en servir.
    #[test]
    fn une_trame_qui_ne_change_que_la_position_compose_les_memes_lignes() {
        let mut e = etat_radio();
        let avant = compose(&e);
        e.position_s = Some(87);
        assert_eq!(compose(&e), avant);
        e.position_s = Some(88);
        assert_eq!(compose(&e), avant);
    }

    /// Et le corollaire sur l'incrustation : pendant un message éphémère, les
    /// trames par seconde composent la même ligne unique, donc la garde
    /// `dernieres_lignes` les absorbe — aucun clignotement pendant que le
    /// message est à l'écran.
    #[test]
    fn une_incrustation_survit_aux_trames_par_seconde() {
        let mut e = etat_radio();
        e.overlay = Some(Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 });
        e.position_s = Some(87);
        let avant = compose(&e);
        e.position_s = Some(88);
        e.overlay = Some(Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e), avant);
    }
```

- [ ] **Étape 2 : voir les tests échouer ou passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-plugin-console"`
Expected: PASS d'emblée — `compose` ne lit pas `position_s`. C'est le résultat voulu : ce test **verrouille** la décision, il ne pilote pas une implémentation. Si l'un des deux échoue, c'est que `compose` a été touché à tort.

- [ ] **Étape 3 : commit**

```bash
git add crates/ritornello-plugin-console/src/display.rs
git commit -m "test(plugin-console): verrouiller que la position ne fait rien reimprimer"
```

---

## Tâche 13 : le parcours de bout en bout

**Files:**
- Modify: `web/app/e2e/parcours.spec.ts`

**Interfaces:**
- Consomme : tout ce qui précède.

- [ ] **Étape 1 : écrire le test qui échoue**

Ajouter au parcours existant, après l'étape qui démarre une lecture :

```ts
  await test.step('la progression apparait et avance', async () => {
    const position = page.locator('[data-position]')
    await expect(position).toBeVisible({ timeout: 15_000 })
    const premiere = await position.textContent()
    // Le coeur publie une trame par seconde pendant la lecture : deux
    // secondes suffisent a voir la valeur bouger, sans rendre le test
    // dependant d'un rythme precis.
    await expect(position).not.toHaveText(premiere ?? '', { timeout: 10_000 })
  })
```

Le parcours joue une **radio** : la position n'y apparaît que si un plugin `metadata` suit la station. Si le montage e2e ne déclare aucun plugin `metadata`, viser plutôt la source `files` du parcours si elle existe, ou marquer cette étape `test.skip` avec un commentaire disant pourquoi — lire `parcours.spec.ts` et `serve.mjs` avant de choisir.

- [ ] **Étape 2 : lancer le parcours**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo build --workspace"` puis, côté Windows, `npm run build --workspaces && npm run e2e -w app`
Expected: PASS.

- [ ] **Étape 3 : commit**

```bash
git add web/app/e2e/parcours.spec.ts
git commit -m "test(e2e): la progression apparait et avance pendant la lecture"
```

---

## Tâche 14 : la documentation dit ce que l'appareil fait

**Files:**
- Modify: `docs/interface.md`
- Modify: `docs/plugins.md`

- [ ] **Étape 1 : écrire les sections**

Dans `docs/interface.md`, section « Web remote and command API », après le paragraphe sur la carte Player :

- Les deux nouveaux champs, `position_s` et `seekable`, ce qu'ils veulent dire et pourquoi `seekable` n'est pas déduit de `duration_s`.
- La cadence : une trame par seconde pendant la lecture, aucune au repos ; l'incrustation voyage dans ces trames sans que son échéance bouge.
- La barre : lecture seule ou cliquable selon `seekable`, pilotable au clavier.
- Les deux touches et leur pas réglable, avec les bornes 1–120 s.

Dans `docs/plugins.md`, section sur le genre `metadata` :

- `Enrichment.position_s` : un écoulé au moment de l'émission, ancré par le cœur, et pourquoi ce n'est pas un horodatage.
- Que `radiofrance-metas` le remplit et que les autres n'ont rien à changer.

Et dans la section de l'afficheur console : qu'il ne montre pas la position, et pourquoi.

- [ ] **Étape 2 : vérifier**

Relire les deux fichiers : aucune affirmation qui ne soit vraie du code écrit.

- [ ] **Étape 3 : commit**

```bash
git add docs/interface.md docs/plugins.md
git commit -m "docs: la position, sa cadence, et les touches de deplacement"
```

---

## Tâche 15 (conditionnelle) : la piste d'un disque, et non le disque

**À faire seulement si la mesure de la tâche 0 montre que `time-pos` est relatif au disque entier.** Sinon, supprimer cette tâche du plan et le dire dans le commit.

**Files:**
- Modify: `crates/ritornello-core/src/player/mpv.rs`
- Modify: `crates/ritornello-core/src/player/mod.rs`

- [ ] **Étape 1 : écrire le test qui échoue**

Dans `mpv.rs`, module `tests` :

```rust
    /// Un `cdda://` ouvert en disque entier expose ses pistes en chapitres :
    /// `time-pos` et `duration` sont alors ceux du **disque**. Publier tels
    /// quels afficherait « 41:12 / 62:30 » sur une piste de trois minutes.
    /// Mesuré sur le Pi (voir la tâche 0 du plan).
    #[test]
    fn la_progression_se_ramene_au_chapitre_courant() {
        let chapitres = serde_json::json!([
            {"title": "1", "time": 0.0},
            {"title": "2", "time": 180.0},
            {"title": "3", "time": 400.0}
        ]);
        let p = dans_le_chapitre(
            Progression { position_s: Some(410.0), duration_s: Some(3750.0) },
            Some(2.0),
            &chapitres,
        );
        assert_eq!(p.position_s, Some(10.0));
        assert_eq!(p.duration_s, Some(3350.0), "du début du chapitre à la fin du disque");
    }

    /// Sans chapitre — un fichier, une entrée de liste de lecture — la
    /// progression passe telle quelle.
    #[test]
    fn sans_chapitre_la_progression_ne_bouge_pas() {
        let p = Progression { position_s: Some(87.0), duration_s: Some(254.0) };
        assert_eq!(dans_le_chapitre(p, None, &serde_json::Value::Null), p);
    }
```

- [ ] **Étape 2 : voir le test échouer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core chapitre"`
Expected: FAIL.

- [ ] **Étape 3 : implémenter**

Écrire `dans_le_chapitre(p: Progression, chapitre: Option<f64>, chapitres: &Value) -> Progression` en fonction pure dans `mpv.rs`, et l'appeler depuis `progression()` après deux `get_property` supplémentaires (`chapter`, `chapter-list`). La durée du chapitre est la borne suivante moins la sienne, ou la durée du disque moins la sienne pour le dernier.

- [ ] **Étape 4 : voir les tests passer**

Run: `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test -p ritornello-core"`
Expected: PASS.

- [ ] **Étape 5 : commit**

```bash
git add crates/ritornello-core/src/player
git commit -m "fix(core): sur un disque ouvert entier, situer dans la piste et non dans le disque"
```

---

## Vérification finale

- [ ] `wsl.exe -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/position-piste && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings"`
- [ ] `npm test --workspaces && npm run typecheck`
- [ ] `npm run build --workspaces` puis `cargo build --workspace` (l'ordre compte : la SPA est embarquée à la compilation)
- [ ] `npm run e2e -w app`
