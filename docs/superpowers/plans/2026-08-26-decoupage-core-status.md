# Découpage de `core.rs` et `status.rs` — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ramener `crates/ritornello-core/src/core.rs` (6 203 lignes, dont 3 881 de tests) et `status.rs` (2 087 lignes) à des modules d'une responsabilité chacun, **sans changer un seul comportement** — un déplacement pur, vérifiable par les tests existants et par `git diff --color-moved`.

**Architecture:** `core.rs` devient `core/mod.rs` (struct `Core<P>`, `new`, la boucle d'ingestion) et des **modules enfants** qui portent chacun un `impl<P: Player> Core<P>` partiel. C'est le mécanisme qui rend le découpage gratuit : en Rust, un champ privé est visible depuis le module qui définit la struct **et ses descendants**, donc aucun `pub(crate)`, aucun accesseur, aucun `EtatVue` intermédiaire n'est nécessaire — les couplages relevés (`publie_etat` lit 25 champs, `persist` traverse tout) restent tels quels, seulement rangés. Les tests suivent leur domaine dans le même fichier (`mod tests` avec `use crate::core::*`).

**Tech Stack:** Rust 2021, `cargo test -p ritornello-core` entre chaque déplacement, `cargo test --workspace` à la fin de chaque tâche, `git diff --color-moved=dimmed-zebra` pour prouver le déplacement pur.

**Spec:** conversation du 2026-08-26 ; carte des blocs relevée le même jour (plages de lignes ci-dessous, **indicatives**).

## Global Constraints

- **Ne commence qu'après la fusion de la session parallèle** (chantier serveur MPD et suites) : un découpage en cours et une feature en cours sur le même fichier de 6 000 lignes, c'est un conflit garanti. Vérifier avant de démarrer : `git log --oneline -5 main` et `git worktree list` — aucun worktree actif ne doit toucher `core.rs`/`status.rs`/`main.rs`.
- **Zéro changement de comportement.** Aucune signature publique modifiée (l'API consommée par `main.rs` : `Core::new`, `handle_event`, `handle_input`, `handle_source_update`, `handle_enrichment`, `pochette_arrivee`, `extraction_arrivee`, `rafraichit_position`, `tick_position`, `overlay_deadline`, `expire_overlay`, `bascule_source`, `remove_source`, `oublie_source_morte`, `cable_source_a_chaud`, `set_locale`, `set_theme`, `set_settings`, `set_audio_device`, `set_metadata_order`, `demarrage`, `retry_stream`, `publie_etat`, `active_source`, `locale_courante`, plus `Cablage`, `MetadataCablage`, `EN`). Si une amélioration saute aux yeux en déplaçant, la **noter** dans le message de commit, pas la faire.
- Un déplacement = un commit. Le diff de chaque commit, lu avec `--color-moved`, ne doit montrer que des blocs déplacés plus les lignes `mod`/`use`.
- Définition de fini = workspace : `cargo test --workspace` et `cargo clippy --workspace --all-targets -- -D warnings` verts à la fin de chaque tâche (pas seulement `-p`).
- Les doc-commentaires voyagent avec leur code ; ceux qui expliquent un *pourquoi* d'architecture sont ce que le dépôt a de plus précieux.

---

### Task 0 : sortir `EN` de `core.rs`

**Files:**
- Create: `crates/ritornello-core/src/i18n.rs`
- Modify: `crates/ritornello-core/src/core.rs` (retirer `pub const EN`), `main.rs` (`mod i18n;`), `admin.rs:241`, `status.rs` (6 sites), `system.rs:966`, `theme.rs:239` (`crate::core::EN` → `crate::i18n::EN`)

**Interfaces:**
- Produces: `pub const EN: &str` dans `crate::i18n`.

Pourquoi d'abord : quatre fichiers importent `crate::core::` **uniquement** pour cette constante. La déplacer coupe ces dépendances d'un coup et fait de `core` un module que seul `main.rs` consomme — la propriété qui rend le reste du découpage local.

- [ ] **Step 1 : déplacer**

`i18n.rs` :

```rust
//! Catalogue anglais embarqué du cœur, base du repli `own → common → clé`.

/// Le catalogue **propre** au cœur, en anglais, embarqué dans le binaire ;
/// les autres langues viennent des packs TOML de `deploy/locales/core/`.
pub const EN: &str = include_str!("locales/en.toml");
```

(Reprendre la déclaration exacte de `core.rs` — vérifier si c'est `include_str!` et quel chemin.) Retirer de `core.rs`, ajouter `mod i18n;` dans `main.rs`, remplacer les sept usages.

- [ ] **Step 2 : vérifier**

Run: `cargo test -p ritornello-core && rg 'core::EN' crates/ritornello-core/src`
Expected: vert, et `rg` ne renvoie rien.

- [ ] **Step 3 : commit**

```bash
git add crates/ritornello-core/src
git commit -m "refactor(core): le catalogue EN dans son module, core n'est plus importe que par main"
```

---

### Task 1 : `core.rs` → `core/mod.rs`, et le premier module enfant (overlays et échéances)

**Files:**
- Rename: `crates/ritornello-core/src/core.rs` → `crates/ritornello-core/src/core/mod.rs`
- Create: `crates/ritornello-core/src/core/echeances.rs`

**Interfaces:**
- Produces: le motif que toutes les tâches suivantes répètent :

```rust
// core/mod.rs
mod echeances;
// core/echeances.rs
use super::*;                       // Core, Player, Overlay, Duration, Instant…
impl<P: Player> Core<P> {
    /* méthodes déplacées, corps inchangés */
}
pub(super) fn prochaine_echeance(/* signature inchangée */) { /* inchangée */ }
#[cfg(test)]
mod tests { use crate::core::*; use super::*; /* tests déplacés */ }
```

- [ ] **Step 1 : renommer sans rien changer**

Run: `git mv crates/ritornello-core/src/core.rs crates/ritornello-core/src/core/mod.rs && cargo test -p ritornello-core`
Expected: vert, rien d'autre à toucher (`mod core;` de `main.rs` résout les deux formes).

- [ ] **Step 2 : déplacer overlays + échéances**

Bloc source (indicatif) : `mod.rs` ~2222-2312 (`show_overlay`, `show_tens_overlay`, `overlay_deadline`, `tick_position`, `expire_overlay`) et la fonction libre `prochaine_echeance` ~2313-2320. Créer `core/echeances.rs` avec le squelette ci-dessus, y coller ces méthodes **telles quelles**. Si `prochaine_echeance` était `fn` privée appelée depuis `mod.rs`, la déclarer `pub(super)`. Déplacer aussi les tests de `mod tests` qui n'exercent que ces méthodes (chercher `prochaine_echeance`, `overlay_deadline`, `expire_overlay`, `tick_position` dans les noms de tests et les corps) dans le `mod tests` du nouveau fichier.

- [ ] **Step 3 : vérifier**

Run: `cargo test -p ritornello-core && git diff --color-moved=dimmed-zebra --stat`
Expected: vert ; le diff montre des blocs déplacés, aucune ligne modifiée hors `mod echeances;`, `use`, et visibilité `pub(super)`.

- [ ] **Step 4 : commit**

```bash
git add -A crates/ritornello-core/src/core
git commit -m "refactor(core): core.rs devient core/mod.rs ; overlays et echeances dans leur module"
```

---

### Task 2 : position et métadonnées/pochettes

**Files:**
- Create: `crates/ritornello-core/src/core/position.rs` (bloc ~1107-1153 : `rafraichit_position`, `oublie_position`)
- Create: `crates/ritornello-core/src/core/metadonnees.rs` (bloc ~729-1106 : `set_identity`, `handle_icy_title`, `handle_file_tags`, `handle_path`, `extraction_arrivee`, `handle_enrichment`, `set_cover_de_source`, `lance_pochette`, `pochette_arrivee`, `app_covers` ; et ~662-728 `applique_selection`, `applique_pochette_de_source`)

Même motif que Task 1. Deux commits, un par fichier. Les tests correspondants (chercher `pochette`, `icy`, `identity`, `enrichment`, `extraction`, `position` dans les noms) suivent.

Point d'attention : `set_identity` est appelé 11 fois depuis d'autres domaines (veille, arrêt, bascule). Il **reste** une méthode de `Core<P>` visible de tout `core::*` — le déplacement ne change pas sa portée. Ne pas essayer de « clarifier la frontière métadonnées/lecture » ici : noter dans le commit.

- [ ] **Step 1 : `position.rs`** — déplacer, `cargo test -p ritornello-core`, commit `refactor(core): la position dans son module`.
- [ ] **Step 2 : `metadonnees.rs`** — déplacer, `cargo test -p ritornello-core`, commit `refactor(core): metadonnees, pochettes et extraction dans leur module`.

---

### Task 3 : superviseur de sources et réglages

**Files:**
- Create: `crates/ritornello-core/src/core/sources.rs` (bloc ~1774-2090 : `active_source`, `locale_courante`, `add_source`, `bascule_source`, `oublie_source_morte`, `remove_source`, `cable_source_a_chaud`, `envoie_locale_a`, `set_metadata_order` ; et ~2091-2144 `apply(SourceAction)`)
- Create: `crates/ritornello-core/src/core/reglages.rs` (bloc ~2145-2221 : `set_audio_device`, `set_locale`, `set_theme`, `persist`)

Même motif. `apply` va avec les sources parce que c'est le retour d'une `SourceAction` ; `persist` va avec les réglages parce que c'est ce qu'il écrit — même s'il lit la veille et le volume (noter, ne pas corriger).

- [ ] **Step 1 : `sources.rs`** — déplacer, tester, commit `refactor(core): le superviseur de sources dans son module`.
- [ ] **Step 2 : `reglages.rs`** — déplacer, tester, commit `refactor(core): les reglages et la persistance dans leur module`.

---

### Task 4 : commandes, volume et événements du lecteur

**Files:**
- Create: `crates/ritornello-core/src/core/commandes.rs` (bloc ~1432-1678 `appliquer_commande` ; ~1303-1341 `handle_command`, `set_volume`, `step_volume` ; ~1342-1431 `handle_input`, `set_settings`, `demarrage`, `start_in_standby`)
- Create: `crates/ritornello-core/src/core/lecteur.rs` (bloc ~1679-1773 `handle_event`, `demande_active` ; ~358-416 `resume`, `retry_stream`)

C'est le plus gros des tests (la machine d'état lecture/veille/dizaines est la plus testée) : le `mod tests` de `commandes.rs` sera probablement > 1 500 lignes à lui seul — c'est acceptable, c'est un fichier de tests d'un seul domaine.

- [ ] **Step 1 : `commandes.rs`** — déplacer, tester, commit `refactor(core): commandes, volume et entree dans leur module`.
- [ ] **Step 2 : `lecteur.rs`** — déplacer, tester, commit `refactor(core): evenements du lecteur et relance dans leur module`.

---

### Task 5 : ingestion des trames et publication — ce qui reste dans `mod.rs`

**Files:**
- Create: `crates/ritornello-core/src/core/publication.rs` (bloc ~1154-1302 : `publie_etat`, `catalogue`, `publie_catalogue`, `etat_lecteur`)
- Modify: `crates/ritornello-core/src/core/mod.rs` — garde : imports, consts, `trait Source`, `EventOutcome`, `Cablage`, `MetadataCablage`, `struct Core<P>`, `new`, `resout_standby_status`, `handle_source_update` + `applique_les_faits_declares`, et les tests transverses (ceux qui traversent plusieurs domaines : bascule + métadonnées + publication, par exemple).

`handle_source_update` (220 lignes) reste dans `mod.rs` **exprès** : elle écrit dans tous les domaines, c'est le point d'entrée d'une trame Source, et la découper serait un changement de conception, pas un déplacement.

- [ ] **Step 1 : `publication.rs`** — déplacer, tester, commit `refactor(core): la publication de l'etat dans son module`.
- [ ] **Step 2 : bilan** — `wc -l crates/ritornello-core/src/core/*.rs` ; cible : `mod.rs` < 1 200 lignes, aucun enfant > 2 000 (tests compris). Commit d'un court `//!` en tête de `mod.rs` qui liste les enfants et leur rôle (une ligne chacun).

---

### Task 6 : `status.rs` — quatre extractions

**Files:**
- Create: `crates/ritornello-core/src/status/reglages_validation.rs` (bloc ~288-501 : constantes de plages, `SettingsError`, `validate_settings`)
- Create: `crates/ritornello-core/src/status/journaux.rs` (bloc ~714-832 : handler logs, `player_sse`, `LogBuffer`, `LogBufferWriter`)
- Create: `crates/ritornello-core/src/status/greffons.rs` (`PluginStatus` + impl ~31-101, `OrdreGreffon`, `GreffonsControle`, `mark_plugin_disconnected`, `replace_plugin_lines`, `plugin_enabled_put`)
- Create: `crates/ritornello-core/src/status/locales.rs` (`parse_available_locales`, `list_locales`, handlers locale/i18n)
- Modify: `status.rs` → `status/mod.rs` : garde `StatusState`, `AppState`, `router`, audio-output, settings GET/PUT, `command`, `tests_support`.

Ici les items sont des fonctions libres et des types, pas des méthodes d'une struct : chaque extraction doit ré-exporter ce que le reste du crate importe (`pub use greffons::{PluginStatus, GreffonsControle, OrdreGreffon};` etc. dans `mod.rs`), pour que `main.rs`, `admin.rs`, `system.rs` n'aient **aucun** import à changer. Vérifier après chaque extraction : `rg 'status::' crates/ritornello-core/src --type rust -l` et compiler.

- [ ] un commit par fichier, dans l'ordre ci-dessus (le plus autonome d'abord) ; `cargo test -p ritornello-core` entre chaque.
- [ ] fin de tâche : `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`.

---

### Task 7 : doc

**Files:**
- Modify: `docs/development.md` (section sur l'organisation du crate `core`, si elle existe ; sinon en créer une courte)

- [ ] Décrire l'arborescence `core/` et `status/` en une ligne par fichier, et **la règle** qui a rendu le découpage possible sans accesseurs : un module enfant voit les champs privés de la struct définie par son parent — donc on ajoute un domaine en ajoutant un fichier avec son `impl<P: Player> Core<P>`, jamais en rendant un champ `pub`.
- [ ] commit `docs(dev): l'arborescence de core et status, et la regle des modules enfants`.

---

## Auto-revue

- **Ce que le plan ne fait pas, volontairement** : découper `handle_source_update`, clarifier `set_identity`, introduire un `EtatVue` pour `publie_etat`. Ce sont des changements de conception ; ils deviendront **possibles** une fois les domaines rangés, et se planifieront séparément.
- **Preuve de non-régression** : les ~984 tests Rust existants, déplacés mais pas modifiés, plus `--color-moved` sur chaque commit.
- **Risque** : les plages de lignes datent du 2026-08-26 ; après la fusion de la session parallèle elles seront fausses — chercher par **nom de méthode**, jamais par numéro de ligne.
