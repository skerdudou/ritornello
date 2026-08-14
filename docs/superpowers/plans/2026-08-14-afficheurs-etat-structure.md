# Afficheurs libres : plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** un plugin d'affichage reçoit l'état structuré de l'appareil et compose
lui-même sa mise en page, au lieu de recevoir trois lignes déjà composées par le
cœur.

**Architecture:** `PlayerState` devient la charge utile unique (SPA en SSE,
afficheurs par socket) et déménage dans `ritornello-proto` pour que le SDK
puisse la désérialiser. Elle gagne un `status` (phrase d'état déjà traduite) et
un `overlay` (enum étiqueté portant donnée *et* texte). Les sources cessent de
composer : `view` et `line2_replaceable` disparaissent du protocole, `View` est
supprimé. La mise en page part vers le plugin console. Le cœur garde tout
l'arbitrage ICY/métadonnées et toutes les échéances.

**Tech Stack:** Rust (tokio, serde, axum), Vue 3 + TypeScript, catalogues i18n
TOML.

**Spec:** `docs/superpowers/specs/2026-08-14-afficheurs-etat-structure-design.md`

## Global Constraints

- **Chaque tâche laisse l'arbre compilable et la suite verte.** Le changement
  est transversal : on ajoute avant de retirer, jamais l'inverse.
- **Les tests de `Metadonnees` (arbitrage ICY/métadonnées) ne doivent pas
  changer.** S'il faut les toucher, le changement a dérivé hors périmètre :
  s'arrêter et le signaler.
- **Clés JSON en anglais** (`status`, `overlay`, `remaining_ms`), par cohérence
  avec `preset_name`, `preset_count`, `duration_s`.
- **Commentaires, messages de commit et tests en français**, accents compris.
  Les doc `///` suivent le fichier : `proto`, `plugin-sdk` et `state.rs`
  documentent leur API publique en anglais.
- **Logs en anglais** (`tracing`, `anyhow`, `.context`).
- **Aucune compatibilité** : pas de champ de version, pas de double chemin. Un
  plugin non mis à jour doit **ne plus compiler**, c'est le signal voulu.
- Toute clé i18n visible existe dans **les deux** catalogues
  (`crates/ritornello-core/src/locales/en.toml` et
  `deploy/locales/core/fr.toml`).
- Commandes cargo par WSL :
  `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/systeme && cargo ..."`.
  npm nativement sous Windows. `npm run build` **avant** cargo, puis
  `touch crates/ritornello-core/build.rs`.
- Playwright : `npm run e2e -w app` (sa config vit dans `web/app`).

---

## Structure des fichiers

| Fichier | Responsabilité après le chantier |
|---|---|
| `crates/ritornello-proto/src/metadata.rs` | `Morceau`, `PlayerState`, `Overlay` — la charge utile partagée |
| `crates/ritornello-proto/src/source.rs` | `SourceMessage` sans `view` ni `line2_replaceable`, avec `status` |
| `crates/ritornello-proto/src/view.rs` | **supprimé** |
| `crates/ritornello-plugin-sdk/src/server.rs` | `SourceOutcome`, `Notification`, `DisplayPlugin::show(PlayerState)` |
| `crates/ritornello-plugin-sdk/src/client.rs` | `SourceUpdate` |
| `crates/ritornello-core/src/metadata.rs` | `Metadonnees` seul — l'arbitrage, plus la composition |
| `crates/ritornello-core/src/core.rs` | état, échéances, publication **unique** |
| `crates/ritornello-core/src/main.rs` | un seul canal relayé vers les afficheurs |
| `crates/ritornello-plugin-console/src/display.rs` | **la mise en page** : trame → trois lignes |
| `crates/ritornello-plugin-radio/src/main.rs` | déclare `status`, ne compose plus |
| `crates/ritornello-plugin-cd/src/main.rs` | déclare `status`, ne compose plus |
| `web/app/src/types.ts`, `PlayerCard.vue` | `status` affiché, `overlay` ignoré |

---

### Task 1 : `PlayerState` et `Morceau` déménagent dans `ritornello-proto`

Déplacement pur, aucun changement de comportement. Nécessaire parce que le SDK
devra désérialiser `PlayerState` pour les afficheurs, et qu'il ne peut pas
dépendre du cœur (dépendance circulaire).

**Files:**
- Modify: `crates/ritornello-proto/src/metadata.rs` (accueille les deux types)
- Modify: `crates/ritornello-core/src/metadata.rs` (les retire, les réimporte)

**Interfaces:**
- Consomme : rien.
- Produit : `ritornello_proto::{Morceau, PlayerState}`, mêmes champs et mêmes
  dérives qu'aujourd'hui (`Debug, Clone, Default, PartialEq, Serialize`).

- [ ] **Step 1: déplacer les deux structures**

Couper `Morceau` et `PlayerState` de `crates/ritornello-core/src/metadata.rs`
(avec **tous** leurs commentaires de documentation, mot pour mot) et les coller
dans `crates/ritornello-proto/src/metadata.rs`. `Morceau::est_vide` est
`#[cfg(test)]` dans le cœur : le déplacer aussi, il sert aux tests du cœur —
donc le rendre `pub fn est_vide` sans `#[cfg(test)]` dans proto, sinon il
disparaît des tests du cœur (un `#[cfg(test)]` ne s'applique qu'au crate qui
compile).

- [ ] **Step 2: réimporter dans le cœur**

En tête de `crates/ritornello-core/src/metadata.rs` :

```rust
pub use ritornello_proto::{Morceau, PlayerState};
```

Ce réexport évite de toucher les dizaines de `use crate::metadata::PlayerState`
existants — le chemin reste valide.

- [ ] **Step 3: vérifier**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Attendu : **exactement** le même nombre de tests qu'avant, tous verts. Un
déplacement pur ne change aucun compte.

- [ ] **Step 4: commit**

```
git add crates/ritornello-proto/src/metadata.rs crates/ritornello-core/src/metadata.rs
git commit -m "refactor(proto): PlayerState et Morceau rejoignent le protocole"
```

---

### Task 2 : le type `Overlay`, et `PlayerState` gagne `status` et `overlay`

Additif : personne ne remplit encore ces champs. C'est la tâche qui pose le
type et son égalité particulière.

**Files:**
- Modify: `crates/ritornello-proto/src/metadata.rs`
- Test: même fichier (module `tests`)

**Interfaces:**
- Consomme : `PlayerState` (Task 1).
- Produit : `ritornello_proto::Overlay`, et `PlayerState.status: Option<String>`
  / `PlayerState.overlay: Option<Overlay>`.

- [ ] **Step 1: écrire les tests qui échouent**

Dans le module `tests` de `crates/ritornello-proto/src/metadata.rs` :

```rust
#[test]
fn overlay_volume_fait_un_aller_retour_json() {
    let o = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
    let json = serde_json::to_string(&o).unwrap();
    // Étiquetage interne : un objet plat, plus simple à lire côté web qu'un
    // couple {"kind":…,"data":{…}}.
    assert!(json.contains("\"kind\":\"volume\""));
    assert!(json.contains("\"level\":65"));
    let back: Overlay = serde_json::from_str(&json).unwrap();
    assert_eq!(back, o);
}

#[test]
fn overlay_cumul_et_message_font_un_aller_retour_json() {
    let t = Overlay::Tens { offset: 20, text: "PRESELECTION +20".into(), remaining_ms: 3000 };
    let json = serde_json::to_string(&t).unwrap();
    assert!(json.contains("\"kind\":\"tens\""));
    assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), t);

    let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 };
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("\"kind\":\"message\""));
    assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), m);
}

#[test]
fn deux_incrustations_ne_differant_que_par_le_temps_restant_sont_egales() {
    // La garantie qui protège la déduplication de `publie_etat` : deux trames
    // qui ne diffèrent que par le temps restant décrivent le même écran. Sans
    // cette égalité, chaque rafraîchissement redondant serait poussé, et
    // chaque afficheur réimprimerait la même chose.
    let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
    let b = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 120 };
    assert_eq!(a, b);
}

#[test]
fn une_incrustation_qui_differe_ailleurs_reste_differente() {
    // Garde-fou de l'égalité ci-dessus : elle ignore le temps restant, et rien
    // d'autre.
    let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
    let b = Overlay::Volume { level: 66, muted: false, text: "VOLUME 66 %".into(), remaining_ms: 4200 };
    assert_ne!(a, b);
    let c = Overlay::Message { text: "X".into(), remaining_ms: 1 };
    let d = Overlay::Message { text: "Y".into(), remaining_ms: 1 };
    assert_ne!(c, d);
}

#[test]
fn les_deux_champs_neufs_sont_absents_du_json_quand_ils_sont_vides() {
    // La charge utile de la SPA ne doit pas se remplir de nulls.
    let json = serde_json::to_string(&PlayerState::default()).unwrap();
    assert!(!json.contains("status"));
    assert!(!json.contains("overlay"));
}
```

- [ ] **Step 2: lancer, constater l'échec**

Run: `cargo test -p ritornello-proto`
Attendu : ÉCHEC de compilation — `Overlay` n'existe pas.

- [ ] **Step 3: implémenter**

Dans `crates/ritornello-proto/src/metadata.rs` :

```rust
/// A transient overlay the appliance is showing right now, carrying **both**
/// the raw value and the resolved words: a display can draw a volume gauge
/// from `level`, or simply print `text`, without needing a catalogue of its
/// own.
///
/// `remaining_ms` is informative. The core alone owns the deadline — it
/// publishes a frame when the overlay expires — so a display may animate a
/// countdown but never decides when the overlay ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Overlay {
    /// Volume/mute overlay.
    Volume { level: u8, muted: bool, text: String, remaining_ms: u32 },
    /// Pending tens offset being composed on the remote (`+10`, `+20`).
    Tens { offset: u8, text: String, remaining_ms: u32 },
    /// Ephemeral message from a source ("empty preset").
    Message { text: String, remaining_ms: u32 },
}

/// Égalité **volontairement écrite à la main** : elle ignore `remaining_ms`.
///
/// Deux incrustations qui ne diffèrent que par le temps restant décrivent le
/// même écran, et `Core::publie_etat` déduplique les trames par égalité. Une
/// dérive automatique ferait passer chaque rafraîchissement redondant pour un
/// changement — plusieurs chemins du cœur rafraîchissent pour un même
/// événement — et chaque afficheur réimprimerait la même chose.
///
/// Écrite ici, sur `Overlay`, et non sur `PlayerState` : au niveau de la
/// charge utile il faudrait comparer à la main tous les autres champs pour ne
/// traiter spécialement qu'un champ imbriqué dans un enum sous une `Option`,
/// et chaque champ ajouté plus tard serait un oubli en puissance.
impl PartialEq for Overlay {
    fn eq(&self, autre: &Self) -> bool {
        match (self, autre) {
            (
                Self::Volume { level: a, muted: ma, text: ta, .. },
                Self::Volume { level: b, muted: mb, text: tb, .. },
            ) => a == b && ma == mb && ta == tb,
            (Self::Tens { offset: a, text: ta, .. }, Self::Tens { offset: b, text: tb, .. }) => {
                a == b && ta == tb
            }
            (Self::Message { text: ta, .. }, Self::Message { text: tb, .. }) => ta == tb,
            _ => false,
        }
    }
}
```

Puis, dans `PlayerState`, après `preset_name` :

```rust
    /// The appliance's current state as a **resolved sentence**: the status a
    /// source declared ("NO DISC", "AUDIO CD") or the core's standby word.
    /// One slot, because there is never more than one status at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The transient overlay showing right now, if any. Displays render it as
    /// they see fit; the SPA ignores it (it shows the volume in plain sight
    /// and has its own toasts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
```

`PlayerState` doit aussi dériver `Deserialize` (le SDK la désérialisera en
Task 4) : ajouter `Deserialize` à sa liste de dérives et à celle de `Morceau`.

Ajouter `Overlay` au `pub use metadata::{…}` de
`crates/ritornello-proto/src/lib.rs`, comme la Task 1 l'a fait pour `Morceau` et
`PlayerState` : sans cette ligne le chemin `ritornello_proto::Overlay` n'existe
pas à la racine du crate, et rien ne le signale avant le premier usage externe.

- [ ] **Step 4: lancer, constater le succès**

Run: `cargo test -p ritornello-proto`
Attendu : les cinq tests passent.

- [ ] **Step 5: le type web**

Dans `web/app/src/types.ts`, sur `PlayerPayload`, après `preset_name` :

```ts
  /**
   * Phrase d'etat deja traduite : le statut declare par la source (« PAS DE
   * DISQUE ») ou le mot de veille resolu par le coeur. `null` quand il n'y a
   * rien a dire.
   */
  status: string | null
  /**
   * Incrustation en cours cote afficheur. La SPA l'ignore — elle montre le
   * volume en clair et a ses propres toasts — mais le champ voyage parce que
   * la charge utile est unique.
   */
  overlay: unknown | null
```

Compléter les fixtures typées qui construisent un `PlayerPayload` complet :
`web/app/src/components/PlayerCard.test.ts`, `web/app/src/views/HomeView.test.ts`,
`web/app/src/composables/usePlayer.test.ts` (`status: null, overlay: null`).

- [ ] **Step 6: vérifier et commiter**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run typecheck
npm test -w app
git add crates/ritornello-proto/src/metadata.rs web/app/src
git commit -m "feat(proto): l'état du lecteur porte un statut et une incrustation"
```

---

### Task 3 : les sources déclarent un `status`, le cœur le fusionne

Additif : les sources continuent d'envoyer leur `view`, donc les afficheurs ne
changent pas de comportement. Ce qui change : la SPA reçoit enfin le statut.

**Files:**
- Modify: `crates/ritornello-proto/src/source.rs` (`SourceMessage.status`)
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (`SourceOutcome`, `Notification`)
- Modify: `crates/ritornello-plugin-sdk/src/client.rs` (`SourceUpdate` + condition de trame non vide)
- Modify: `crates/ritornello-core/src/core.rs` (champ, fusion, veille, publication)
- Modify: `crates/ritornello-plugin-radio/src/main.rs`, `crates/ritornello-plugin-cd/src/main.rs`
- Modify: `web/app/src/components/PlayerCard.vue`

**Interfaces:**
- Consomme : `PlayerState.status` (Task 2).
- Produit : `SourceMessage.status`, `SourceOutcome::status(impl Into<String>)`,
  `SourceUpdate.status`, `Notification.status`.

- [ ] **Step 1: écrire les tests qui échouent (cœur)**

Dans le module `tests` de `crates/ritornello-core/src/core.rs`, en réutilisant
les fabriques d'`update` existantes :

```rust
#[tokio::test]
async fn un_statut_de_source_est_publie_puis_remplace() {
    // Convention **différente** de celle de `preset` : dans une trame,
    // `status` absent signifie « aucun statut », pas « garder le précédent ».
    // C'est ce qui reproduit le comportement actuel — une source recompose sa
    // vue entière à chaque trame — et la seule convention qui permette
    // d'effacer un statut : sinon « PAS DE DISQUE » resterait affiché après
    // l'insertion d'un disque, sans aucune façon de l'annuler.
    let (mut core, _pc, _sc, _rx, _d) = setup();
    let mut update = update_nu();
    update.status = Some("PAS DE DISQUE".into());
    core.handle_source_update("radio", update);
    assert_eq!(core.etat_lecteur().status.as_deref(), Some("PAS DE DISQUE"));

    core.handle_source_update("radio", update_nu());
    assert_eq!(core.etat_lecteur().status, None, "absent vaut effacé, pas conservé");
}

#[tokio::test]
async fn un_statut_ephemere_ne_touche_pas_au_statut_memorise() {
    // Le cas « présélection vide » : un mot passager, alors que la station
    // précédente continue de jouer. Il alimente l'incrustation, et le statut
    // permanent doit reparaître à l'échéance.
    let (mut core, _pc, _sc, _rx, _d) = setup();
    let mut permanent = update_nu();
    permanent.status = Some("FIP".into());
    core.handle_source_update("radio", permanent);

    let mut ephemere = update_nu();
    ephemere.status = Some("PRESELECTION VIDE".into());
    ephemere.transient = true;
    core.handle_source_update("radio", ephemere);
    assert_eq!(
        core.etat_lecteur().status.as_deref(),
        Some("FIP"),
        "le statut permanent survit à un message éphémère"
    );
    assert!(matches!(core.etat_lecteur().overlay, Some(Overlay::Message { .. })));

    core.expire_overlay();
    assert_eq!(core.etat_lecteur().status.as_deref(), Some("FIP"));
    assert!(core.etat_lecteur().overlay.is_none());
}
```

`update_nu()` est la fabrique d'une `SourceUpdate` sans rien (tous les champs à
`None`/`false`) : si elle n'existe pas déjà sous ce nom dans le module de
tests, l'ajouter à côté des fabriques existantes.

- [ ] **Step 2: lancer, constater l'échec**

Run: `cargo test -p ritornello-core`
Attendu : ÉCHEC de compilation — `SourceUpdate` n'a pas de champ `status`.

- [ ] **Step 3: le champ, du protocole au cœur**

`crates/ritornello-proto/src/source.rs`, dans `SourceMessage`, après
`preset_name` :

```rust
    /// The source's own word about its state, **already translated** by its
    /// catalogue ("NO DISC", "AUDIO CD", "EMPTY PRESET").
    ///
    /// Unlike `preset`, absent means **"no status"**, not "keep the previous
    /// one": a source restates it on every frame, and this is the only
    /// convention that lets a status be cleared at all.
    ///
    /// With `transient` set, the status is an ephemeral message: it feeds the
    /// overlay and leaves the remembered status untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
```

`crates/ritornello-plugin-sdk/src/server.rs` : le même champ sur
`SourceOutcome` **et** sur `Notification`, le constructeur fluide, et la
propagation dans la conversion vers `SourceMessage`. `Notification` reçoit au
passage `preset_name`, qui lui manquait.

```rust
    /// Declares the source's own state word (see `SourceMessage::status`).
    pub fn status(mut self, mot: impl Into<String>) -> Self {
        self.status = Some(mot.into());
        self
    }
```

`crates/ritornello-plugin-sdk/src/client.rs` : le champ sur `SourceUpdate`, sa
propagation, **et la condition de trame non vide** (vers la ligne 86) qui doit
inclure `msg.status.is_some()` — sans quoi une trame ne portant qu'un statut
serait silencieusement jetée.

`crates/ritornello-core/src/core.rs` :

```rust
    /// Statut permanent déclaré par la Source active, déjà traduit (voir
    /// `SourceMessage::status`). Remplacé à chaque trame non éphémère, y
    /// compris par son absence — voir le test de convention.
    source_status: Option<String>,
```

Dans `handle_source_update`, **avant** le traitement de l'incrustation :

```rust
        // `status` est réaffirmé par chaque trame permanente : absent vaut
        // effacé. Une trame éphémère, elle, ne touche pas au statut mémorisé —
        // son mot va dans l'incrustation.
        if !update.transient {
            self.source_status = update.status.clone();
        }
```

Et dans `etat_lecteur` :

```rust
            // La veille gagne sur le statut de la source : l'appareil dort, ce
            // que raconte la source n'a plus cours.
            status: if self.standby { self.standby_status.clone() } else { self.source_status.clone() },
```

`standby_status` est le mot de veille résolu, à mémoriser quand la veille est
posée (le catalogue se lit en `async`, `etat_lecteur` ne l'est pas) : ajouter un
champ `standby_status: Option<String>` renseigné là où `standby_view()` était
appelé, avec `cat.get("standby")`.

- [ ] **Step 4: le chemin éphémère vers l'incrustation**

Toujours dans `handle_source_update`, là où une vue éphémère posait
l'incrustation, poser désormais :

```rust
            self.overlay = Some((
                Overlay::Message { text: mot, remaining_ms: self.settings.overlay_ms },
                Instant::now() + Duration::from_millis(self.settings.overlay_ms.into()),
            ));
```

À ce stade `self.overlay` est encore un `(View, Instant)` : cette tâche le
**convertit** en `(Overlay, Instant)`, et `push_view` compose alors une `View`
depuis le texte de l'incrustation (`line1` = le texte, les deux autres vides) —
état transitoire, supprimé en Task 4 avec `push_view` lui-même. `show_overlay`
et `show_tens_overlay` construisent `Overlay::Volume` / `Overlay::Tens`, en
joignant leurs deux anciennes lignes d'un espace (« VOLUME 65 % »,
« PRESELECTION +20 »).

`etat_lecteur` publie l'incrustation avec un `remaining_ms` **frais**, calculé
depuis l'échéance stockée :

```rust
            overlay: self.overlay.as_ref().map(|(o, echeance)| {
                let restant = echeance.saturating_duration_since(Instant::now()).as_millis();
                // Le `remaining_ms` mémorisé n'est jamais lu : il est réécrit
                // ici à chaque publication. L'égalité d'`Overlay` l'ignore, donc
                // ce rafraîchissement ne défait pas la déduplication.
                o.clone().avec_restant(u32::try_from(restant).unwrap_or(u32::MAX))
            }),
```

`avec_restant` est une méthode de `Overlay`, à écrire dans proto avec le type :

```rust
impl Overlay {
    /// Réécrit le temps restant, calculé à la publication depuis l'échéance que
    /// le cœur détient. Le `remaining_ms` mémorisé dans `self` n'est donc jamais
    /// lu — et l'égalité l'ignorant, ce rafraîchissement ne défait pas la
    /// déduplication des trames.
    #[must_use]
    pub fn avec_restant(self, restant_ms: u32) -> Self {
        match self {
            Self::Volume { level, muted, text, .. } => {
                Self::Volume { level, muted, text, remaining_ms: restant_ms }
            }
            Self::Tens { offset, text, .. } => Self::Tens { offset, text, remaining_ms: restant_ms },
            Self::Message { text, .. } => Self::Message { text, remaining_ms: restant_ms },
        }
    }
}
```

- [ ] **Step 5: les sources déclarent leurs statuts**

`crates/ritornello-plugin-radio/src/main.rs` : sur la branche « présélection
vide », ajouter `.status(empty)` à côté de `.transient()`. Sur la branche qui
joue, aucun statut — `preset_name` porte déjà le nom de la station.

`crates/ritornello-plugin-cd/src/main.rs` : `.status(self.catalog.get("no_disc"))`
quand aucun disque n'est présent, `.status(self.catalog.get("cd_audio"))` sinon.
Les vues restent en place pour l'instant.

- [ ] **Step 6: la carte web affiche le statut**

`web/app/src/components/PlayerCard.vue`, après la ligne de présélection :

```html
      <!-- Le statut de la source, déjà traduit par elle. Invisible sur le web
           jusqu'ici pour la même raison que le nom de station l'était : il
           n'existait que dans une ligne d'afficheur. -->
      <p v-if="etat?.status" class="text-sm text-muted-foreground">
        <span class="text-foreground" data-player-status>{{ etat.status }}</span>
      </p>
```

Test dans `web/app/src/components/PlayerCard.test.ts` :

```ts
  it('affiche le statut déclaré par la source', () => {
    const w = monteAvec({ status: 'PAS DE DISQUE' })
    expect(w.find('[data-player-status]').text()).toBe('PAS DE DISQUE')
  })

  it('n affiche aucune ligne de statut quand il n y en a pas', () => {
    const w = monteAvec({ status: null })
    expect(w.find('[data-player-status]').exists()).toBe(false)
  })
```

- [ ] **Step 7: vérifier et commiter**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm test -w app && npm run typecheck
git add crates web
git commit -m "feat: les sources déclarent un statut, la SPA l'affiche"
```

---

### Task 4 : les afficheurs reçoivent la trame, le plugin console compose

Le cœur de l'affaire. À la fin de cette tâche, l'affichage console doit
ressembler **exactement** à ce qu'il était : c'est le critère d'acceptation.

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (`DisplayPlugin::show`)
- Modify: `crates/ritornello-core/src/core.rs` (suppression de `push_view`, de `view_tx`)
- Modify: `crates/ritornello-core/src/main.rs` (un seul canal)
- Modify: `crates/ritornello-core/src/metadata.rs` (suppression de `composer` et `ligne_titre`)
- Modify: `crates/ritornello-plugin-console/src/display.rs` (la mise en page arrive)
- Modify: `crates/ritornello-plugin-console/src/main.rs`

**Interfaces:**
- Consomme : `PlayerState` avec `status` et `overlay` (Tasks 2, 3).
- Produit : `DisplayPlugin::show(&mut self, state: PlayerState)`, et
  `console::display::compose(&PlayerState) -> [String; 3]`.

- [ ] **Step 1: écrire les tests de mise en page (plugin console)**

Dans `crates/ritornello-plugin-console/src/display.rs`, module `tests`. Ces
tests **remplacent** ceux qui construisaient une `View`, et accueillent
`ligne_titre` déplacé depuis le cœur :

```rust
    fn etat_radio() -> PlayerState {
        PlayerState {
            source: "radio".into(),
            volume: 60,
            preset: Some(3),
            preset_name: Some("France Inter".into()),
            ..Default::default()
        }
    }

    #[test]
    fn compose_la_source_et_la_preselection_sur_la_premiere_ligne() {
        let l = compose(&etat_radio());
        assert_eq!(l[0], "RADIO  P3");
        assert_eq!(l[1], "France Inter");
    }

    #[test]
    fn les_quatre_replis_de_la_ligne_de_titre() {
        // Déplacés depuis le cœur avec la fonction qu'ils testent.
        assert_eq!(ligne_titre(Some("Miles Davis"), Some("So What")).as_deref(), Some("Miles Davis — So What"));
        assert_eq!(ligne_titre(None, Some("So What")).as_deref(), Some("So What"));
        // Décision du propriétaire : on affiche toute information disponible,
        // même partielle.
        assert_eq!(ligne_titre(Some("Miles Davis"), None).as_deref(), Some("Miles Davis"));
        assert_eq!(ligne_titre(None, None), None);
    }

    #[test]
    fn l_album_prime_sur_le_statut_quand_les_deux_existent() {
        // Ce que `line2_replaceable` négociait autrefois : le plugin décide,
        // sans avoir à demander la permission au cœur.
        let mut e = PlayerState { source: "cd".into(), preset: Some(1), preset_count: Some(3), ..Default::default() };
        e.status = Some("AUDIO CD".into());
        assert_eq!(compose(&e)[1], "AUDIO CD");
        e.morceau.album = Some("Kind of Blue".into());
        assert_eq!(compose(&e)[1], "Kind of Blue");
    }

    #[test]
    fn une_incrustation_prend_toute_la_place() {
        let mut e = etat_radio();
        e.overlay = Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 });
        assert_eq!(compose(&e)[0], "VOLUME 65 %");
        assert_eq!(compose(&e)[1], "");
    }

    #[test]
    fn la_veille_affiche_son_mot_seul() {
        let e = PlayerState { standby: true, status: Some("VEILLE".into()), ..Default::default() };
        assert_eq!(compose(&e)[0], "VEILLE");
    }

    #[test]
    fn tout_le_contenu_est_assaini_pas_seulement_la_troisieme_ligne() {
        // Depuis que le plugin compose, **chaque** morceau vient du réseau : un
        // nom de station configuré à distance, un statut, un titre ICY. Un flux
        // qui glisserait `\x1b[` dans l'un d'eux pourrait manipuler la console.
        let e = PlayerState {
            source: "radio".into(),
            preset: Some(1),
            preset_name: Some("FI\x1b[2JP".into()),
            ..Default::default()
        };
        let s = render_console(&e);
        assert!(!s.contains("FI\x1b[2JP"));
        assert_eq!(s.matches('\x1b').count(), 2, "seuls les deux ESC de l'en-tête du rendu");
    }
```

- [ ] **Step 2: lancer, constater l'échec**

Run: `cargo test -p ritornello-plugin-console`
Attendu : ÉCHEC — `compose` et `ligne_titre` n'existent pas.

- [ ] **Step 3: implémenter la mise en page**

Dans `crates/ritornello-plugin-console/src/display.rs` :

```rust
/// Trois lignes pour un écran texte d'environ vingt colonnes, composées depuis
/// l'état structuré.
///
/// C'est **ici** que vit la mise en page, et non dans le cœur : un autre
/// afficheur en écrira une autre à partir de la même trame, sans rien changer
/// au cœur.
pub fn compose(etat: &PlayerState) -> [String; 3] {
    // Une incrustation prend toute la place : elle est passagère et c'est ce
    // qu'on veut lire pendant qu'elle dure.
    if let Some(o) = &etat.overlay {
        return [texte_incrustation(o).to_string(), String::new(), String::new()];
    }
    if etat.standby {
        return [etat.status.clone().unwrap_or_default(), String::new(), String::new()];
    }
    let line1 = match etat.preset {
        Some(n) => format!("{}  P{n}", etat.source.to_uppercase()),
        None => etat.source.to_uppercase(),
    };
    // Le nom de la présélection d'abord, puis l'album, puis le statut : du plus
    // spécifique au plus générique.
    let line2 = etat
        .preset_name
        .clone()
        .or_else(|| etat.morceau.album.clone())
        .or_else(|| etat.status.clone())
        .unwrap_or_default();
    let line3 = ligne_titre(etat.morceau.artist.as_deref(), etat.morceau.title.as_deref())
        .unwrap_or_default();
    [line1, line2, line3]
}

fn texte_incrustation(o: &Overlay) -> &str {
    match o {
        Overlay::Volume { text, .. } | Overlay::Tens { text, .. } | Overlay::Message { text, .. } => text,
    }
}

/// Ligne « artiste — titre », avec ses quatre replis. Déplacée du cœur : c'est
/// une décision de mise en page, donc elle appartient à l'afficheur.
pub fn ligne_titre(artist: Option<&str>, title: Option<&str>) -> Option<String> {
    match (artist, title) {
        (Some(a), Some(t)) => Some(format!("{a} — {t}")),
        (None, Some(t)) => Some(t.to_string()),
        (Some(a), None) => Some(a.to_string()),
        (None, None) => None,
    }
}
```

`render_console` prend désormais `&PlayerState`, appelle `compose`, et
**assainit chacune des trois lignes** comme avant.

Attention à l'album : quand une présélection est nommée (radio), `preset_name`
gagne — c'est le comportement actuel, où la station occupait `line2` et l'album
ne la remplaçait pas (la radio ne déclarait pas `line2_replaceable`). Pour le cd,
`preset_name` est absent, donc l'album gagne sur le statut : c'est exactement ce
que `line2_replaceable` produisait.

- [ ] **Step 4: le protocole d'affichage**

`crates/ritornello-plugin-sdk/src/server.rs` : `DisplayPlugin::show` prend
`PlayerState`. `run_display_plugin` désérialise une `PlayerState` par ligne au
lieu d'une `View`.

`crates/ritornello-plugin-console/src/main.rs` : `show(&mut self, state: PlayerState)`.

- [ ] **Step 5: un seul canal dans le cœur**

`crates/ritornello-core/src/core.rs` :
- supprimer `push_view`, `view_tx` du champ et de `Cablage`, ainsi que le
  commentaire « chacun son chemin » de `MetadataCablage` ;
- **chaque appel à `push_view()` devient `publie_etat()`** ;
- supprimer `standby_view()` (remplacé par `standby_status` en Task 3).

Les champs `view` et `view_line2_replaceable` de `Core` deviennent morts ici
(seul `push_view` les lisait) mais **restent en place jusqu'à la Task 5**, qui
les retire avec le reste de la mise en page : `handle_source_update` les alimente
encore tant que les sources envoient une `view`, et un champ écrit jamais lu ne
fait pas échouer `-D warnings` (contrairement à un champ jamais écrit).

`crates/ritornello-core/src/metadata.rs` : supprimer `composer` et
`ligne_titre` avec leurs tests (déplacés en Step 1). **Ne pas toucher à
`Metadonnees`.**

`crates/ritornello-core/src/main.rs` : supprimer le `watch::channel(View::default())`
(vers la ligne 85) ; la boucle de relais vers les afficheurs (vers la ligne 280)
lit le canal d'état — le même que le flux SSE — et envoie une `PlayerState`.

- [ ] **Step 6: vérifier, y compris à l'œil**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Puis, manuellement, avec un cœur lancé selon `docs/development.md` et
`RITORNELLO_CONSOLE_TTY=/dev/stdout` : l'affichage doit être **identique** à
avant le chantier — même première ligne, même station, même ligne de titre, et
les incrustations volume et `+10` doivent apparaître et disparaître comme avant.
Le rapport doit dire ce qui a été observé.

- [ ] **Step 7: commit**

```
git add crates
git commit -m "feat: les afficheurs reçoivent l'état, le plugin console compose"
```

---

### Task 5 : `view` et `line2_replaceable` quittent le protocole, `View` disparaît

Suppression pure : plus personne ne les lit après la Task 4.

**Files:**
- Modify: `crates/ritornello-proto/src/source.rs`, `lib.rs`
- Delete: `crates/ritornello-proto/src/view.rs`
- Modify: `crates/ritornello-plugin-sdk/src/{server.rs,client.rs}`
- Modify: `crates/ritornello-core/src/core.rs`
- Modify: `crates/ritornello-plugin-radio/src/main.rs`, `crates/ritornello-plugin-cd/src/main.rs`

**Interfaces:**
- Consomme : tout ce qui précède.
- Produit : un protocole de source sans aucune mise en page.

- [ ] **Step 1: retirer les champs**

`SourceMessage`, `SourceOutcome`, `SourceUpdate`, `Notification` perdent `view`
et `line2_replaceable`, ainsi que les constructeurs `with_view`,
`line2_replaceable` et `Notification::view`. Supprimer
`crates/ritornello-proto/src/view.rs` et sa ligne dans `lib.rs`.

- [ ] **Step 2: nettoyer les sources**

`radio` : `view_for` et `identite_du_flux` — la première disparaît, la seconde
reste (elle produit l'identité, pas de l'affichage). Les `with_view(...)` s'en
vont ; ce que la vue disait est déjà déclaré (`preset`, `preset_name`,
`status`).

`cd` : `view()` disparaît ; `line1` était « CD n/total », déjà exprimé par
`preset` et `preset_count` que le plugin déclare déjà.

Leurs tests de vue deviennent des tests de statut, ou disparaissent quand ils ne
testaient que la composition.

- [ ] **Step 3: nettoyer le cœur**

Supprimer le champ `view`, `view_line2_replaceable`, et tout ce qui les
alimentait dans `handle_source_update`.

- [ ] **Step 4: vérifier et commiter**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm test && npm run typecheck
npm run build && touch crates/ritornello-core/build.rs && cargo build --workspace
npm run e2e -w app
git add crates
git commit -m "refactor: le protocole des sources ne porte plus de mise en page"
```

---

### Task 6 : documentation

**Files:**
- Modify: `docs/plugins.md` (protocoles source et affichage)
- Modify: `docs/interface.md` (charge utile de `/api/player`)

- [ ] **Step 1: réécrire les sections concernées**

En anglais, comme ces fichiers. Y dire :
- ce qu'un plugin d'affichage reçoit désormais, et qu'il compose lui-même ;
- que la charge utile est **unique** (SSE pour la SPA, socket pour les
  afficheurs) ;
- la convention du `status` (absent = aucun statut, contrairement à `preset`),
  et le cas éphémère ;
- que le cœur reste seul maître des échéances, `remaining_ms` étant indicatif ;
- que `SetLocale` n'existe pas pour les afficheurs, et pourquoi c'est un ajout
  non cassant le jour où il faudra ;
- que `view` et `line2_replaceable` n'existent plus.

- [ ] **Step 2: commiter**

```
git add docs
git commit -m "docs: protocole d'affichage par état structuré"
```

---

## Notes pour le contrôleur

- **Ordre impératif.** Chaque tâche laisse l'arbre vert ; l'inverser casserait
  la compilation entre deux commits.
- **Task 4 est la seule à risque de régression visible** (l'affichage console).
  Son critère d'acceptation est « identique à avant », vérifié à l'œil en plus
  des tests.
- **Signal de dérive :** si une tâche doit modifier les tests de `Metadonnees`,
  s'arrêter et le signaler — le changement est sorti de son périmètre.
