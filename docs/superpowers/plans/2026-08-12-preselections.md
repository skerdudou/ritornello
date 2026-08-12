# Real preset numbers and +10 access — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sources declare their preset count (radio stations, cd tracks); the web grid shows only existing numbers with a +10 shifted window; the physical remote gains `+10` and `0` keys via a core-held tens offset; rider: disable the audio "Change" button when its GET failed.

**Architecture:** `preset_count: Option<u8>` threads through the exact same path as the existing `preset` field (proto `SourceMessage` → SDK builders/`SourceUpdate` → `Core` memory → `PlayerState` → SSE → `PlayerPayload`). A new `Command::Plus10` plus legalized `Select(0)` feed a `pending_tens` offset in the core, displayed through the existing overlay slot and deadline. The web grid derives a decade window from the count, all locally.

**Tech Stack:** Rust workspace (axum core, plugin SDK over Unix sockets), Vue 3 + Vitest + Playwright SPA embedded by `crates/ritornello-core/build.rs`.

**Spec:** `docs/superpowers/specs/2026-08-12-preselections-design.md` (read it once before starting a task if anything below seems ambiguous — the spec governs).

## Global Constraints

- **Environment (critical):** cargo/clippy run ONLY through WSL from PowerShell: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/preselections && cargo test -p <crate>"`. npm/node/npx/Playwright run natively on Windows (PowerShell). Git Bash is broken in this session — use PowerShell for everything else.
- **SPA embedding:** `npm run build` (in `web/app`) BEFORE any `cargo build`/e2e; after an npm rebuild, `touch crates/ritornello-core/build.rs` (via WSL: `touch` inside the same `bash -lc`) so cargo re-embeds the dist.
- **Language policy:** docs in English; Rust test names in French; comments in new Rust code in English; SPA comments in French; user-facing diagnostic strings in French via i18n catalogs. Every new i18n key goes in BOTH the crate's embedded `en.toml` AND the matching `deploy/locales/<plugin-or-core>/fr.toml`.
- **Exact values:** core `OVERLAY` stays 2 s; web window auto-return = 2000 ms; radio preset validation bound = 1..=99; generic-input `Select` binding bound = 0..=9; core tens offset wraps past `(count / 10) * 10` when the count is known, saturates at 240 when unknown; `Select` effective number = `pending_tens + digit`, 0 silently ignored.
- **Never** use bare `git stash`. One commit (or more) per task, French commit messages like the repo's history.
- `Some(0)` for `preset_count` is meaningful ("nothing to number", cd without disc) and distinct from `None` ("nothing declared" → SPA falls back to the 1–9 grid).

---

### Task 0: Prepare the worktree environment

**Files:** none (environment only).

- [ ] **Step 1: Install npm dependencies everywhere.** From `C:\projets\perso\ritornello\.claude\worktrees\preselections`, list package roots: `git ls-files "*package.json" | grep -v node_modules` (PowerShell: `git ls-files *package.json`). Run `npm install` in each directory that has a `package.json` and no `node_modules` (expected: `web/app`, `web/kit`, and each plugin `ui/` dir such as `crates/ritornello-plugin-generic-input/ui`, `crates/ritornello-plugin-radio/ui`).
- [ ] **Step 2: Build the SPA.** In `web/app`: `npm run build`. Expected: dist produced without errors.
- [ ] **Step 3: Build the workspace (WSL).** `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/preselections && touch crates/ritornello-core/build.rs && cargo build --workspace"`. Expected: success (plugin binaries are needed later by e2e).
- [ ] **Step 4: Sanity-run the test suites once.** `npm test` (or `npx vitest run`) in `web/app`; `wsl.exe -- bash -lc "... && cargo test --workspace"`. Both green (this is the baseline). If Playwright's chromium is missing (`npx playwright test --list` errors), run `npx playwright install chromium`.
- [ ] **Step 5: No commit** (nothing tracked changed).

---

### Task 1: Protocol — `Command::Plus10`, legal `Select(0)`, `preset_count` through proto and SDK

**Files:**
- Modify: `crates/ritornello-proto/src/command.rs`
- Modify: `crates/ritornello-proto/src/source.rs`
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (SourceOutcome + Notification builders, SourceMessage mapping)
- Modify: `crates/ritornello-plugin-sdk/src/client.rs` (SourceUpdate + reader gate)
- Modify (mechanical ripple): `crates/ritornello-core/src/core.rs` test literals

**Interfaces:**
- Consumes: nothing (first code task).
- Produces: `Command::Plus10` (serializes `{"cmd":"Plus10"}`); `SourceMessage.preset_count: Option<u8>`; `SourceOutcome::preset_count(n) -> Self` and `Notification::preset_count(n) -> Self` builder methods; `SourceUpdate.preset_count: Option<u8>`. Later tasks rely on these exact names.

- [ ] **Step 1: Write the failing proto tests.** In `crates/ritornello-proto/src/command.rs` tests module:

```rust
#[test]
fn plus10_et_select_zero_font_le_tour() {
    // Plus10 est la touche +10 de la télécommande, Select(0) sa touche 0 :
    // les deux doivent voyager tels quels.
    let p = Command::Plus10;
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(json, r#"{"cmd":"Plus10"}"#);
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), p);
    let z = Command::Select(0);
    let json = serde_json::to_string(&z).unwrap();
    assert_eq!(json, r#"{"cmd":"Select","arg":0}"#);
    assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), z);
}
```

In `crates/ritornello-proto/src/source.rs` tests module (mirror of `la_selection_fait_le_tour_et_reste_absente_par_defaut`):

```rust
#[test]
fn le_compte_fait_le_tour_et_reste_absent_par_defaut() {
    let m = SourceMessage {
        id: Some(3),
        action: Some(SourceAction::Noop),
        view: None,
        identity: None,
        line2_replaceable: false,
        transient: false,
        preset: None,
        preset_count: Some(23),
    };
    let json = serde_json::to_string(&m).unwrap();
    assert!(json.contains("\"preset_count\":23"));
    let back: SourceMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(back.preset_count, Some(23));
    // Trame d'un plugin antérieur : rien déclaré.
    let ancien: SourceMessage = serde_json::from_str(r#"{"id":3}"#).unwrap();
    assert_eq!(ancien.preset_count, None);
    // Some(0) est porteur de sens (cd sans disque) et doit voyager tel quel,
    // distinct de l'absence.
    let zero: SourceMessage = serde_json::from_str(r#"{"id":3,"preset_count":0}"#).unwrap();
    assert_eq!(zero.preset_count, Some(0));
}
```

- [ ] **Step 2: Run to verify failure.** `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/preselections && cargo test -p ritornello-proto"`. Expected: compile errors (`Plus10` not found, missing field).

- [ ] **Step 3: Implement proto.** In `command.rs`, add the variant right after `Select(u8)`:

```rust
    Select(u8),
    /// Cumulative tens key of the remote: each press shifts the next digit
    /// key by +10 (`+10` then `4` selects 14, `+10 +10` then `0` selects 20).
    /// The pending offset lives in the core — which also displays it and
    /// expires it; input plugins just relay the key press.
    Plus10,
```

In `source.rs`, add the field right after `preset`:

```rust
    /// How many numbered presets the source currently offers: stations for
    /// the radio, tracks for the cd. This is what lets the web UI show only
    /// the numbers that exist instead of an unconditional 1-9 grid.
    ///
    /// Absent = "this frame says nothing about the count, keep the previous
    /// one". `Some(0)` is meaningful — "there is nothing to number" (cd
    /// without a disc) — and distinct from absent. The core forgets the
    /// remembered count on source change and standby (the next source
    /// re-declares it on activate/wake), but NOT on stop: a stopped radio
    /// still has its stations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_count: Option<u8>,
```

Add `preset_count: None` to every existing `SourceMessage` literal in the `source.rs` tests. The exact-JSON test `identite_absente_nest_pas_serialisee` stays valid (the field is skipped when `None`).

- [ ] **Step 4: Run proto tests.** Same command as Step 2. Expected: PASS.

- [ ] **Step 5: Write the failing SDK tests.** In `crates/ritornello-plugin-sdk/src/server.rs` tests (mirror how existing builder tests exercise `.preset(n)` — read the neighbors first):

```rust
#[test]
fn le_compte_du_builder_atterrit_dans_la_trame() {
    let o = SourceOutcome::new(SourceAction::Noop).preset_count(23);
    assert_eq!(o.preset_count, Some(23));
    let n = Notification::new().preset_count(0);
    assert_eq!(n.preset_count, Some(0));
}
```

(If `Notification::new()` has a different constructor name, mirror the neighboring Notification tests — the assertion is what matters.) In `crates/ritornello-plugin-sdk/src/client.rs` tests, mirror the existing test that checks a `SourceMessage` maps into a `SourceUpdate` (there is one for `preset`); assert a message carrying only `preset_count: Some(5)` produces an update with `preset_count == Some(5)` — i.e. a count-only frame is "interesting".

- [ ] **Step 6: Run to verify failure**, then **implement the SDK threading**:
  - `SourceOutcome` and `Notification` (in `server.rs`): add `pub preset_count: Option<u8>` field (default `None` in constructors) and builder method on both:

```rust
    /// Declare how many numbered presets exist after this frame (stations,
    /// tracks). See `SourceMessage::preset_count` for the exact semantics.
    pub fn preset_count(mut self, n: u8) -> Self {
        self.preset_count = Some(n);
        self
    }
```

  - In `run_source_plugin` (server.rs), copy the field into the emitted `SourceMessage` exactly where `preset` is copied (`preset_count: outcome.preset_count`) — both for outcomes and notifications.
  - `SourceUpdate` (client.rs:39-51): add `pub preset_count: Option<u8>`.
  - Reader gate (client.rs:84-92): extend the condition to `msg.view.is_some() || msg.identity.is_some() || msg.preset.is_some() || msg.preset_count.is_some()` and copy the field into the `SourceUpdate` literal.

- [ ] **Step 7: Fix the mechanical ripple.** `wsl.exe -- bash -lc "... && cargo test --workspace"` — every `SourceUpdate { ... }` literal in `crates/ritornello-core/src/core.rs` tests (around lines 894, 899, 908, 1731) now misses the field: add `preset_count: None` to each (do NOT use `..Default::default()` where the existing style writes fields out). Expected: workspace green.

- [ ] **Step 8: Commit.** `git add -A && git commit -m "feat(proto,sdk): commande Plus10 et compte de présélections déclaré par les sources"`

---

### Task 2: Core — remember/forget the count, publish it in `PlayerState`

**Files:**
- Modify: `crates/ritornello-core/src/core.rs`
- Modify: `crates/ritornello-core/src/metadata.rs`

**Interfaces:**
- Consumes: `SourceUpdate.preset_count` (Task 1).
- Produces: `Core.preset_count: Option<u8>` private field (Task 3 reads it for the wrap rule); `PlayerState.preset_count: Option<u8>` (Task 7's SSE payload).

- [ ] **Step 1: Write the failing tests** in `core.rs`'s tests module. Read the neighboring tests first (the fake-source harness, how `handle_source_update` is fed with `SourceUpdate` literals, how `etat_lecteur()` is asserted — e.g. the tests around the `preset` memory). Then add, following the same harness:

```rust
#[tokio::test]
async fn le_compte_de_preselections_est_memorise_et_publie() {
    // Une trame qui déclare un compte doit se retrouver dans PlayerState ;
    // une trame muette sur le sujet ne doit pas l'effacer.
    // [construire le cœur comme les tests voisins, puis :]
    core.handle_source_update("radio", update_avec_compte(Some(23))).await; // adapter au helper local
    assert_eq!(core.etat_lecteur().preset_count, Some(23));
    core.handle_source_update("radio", update_avec_compte(None)).await;
    assert_eq!(core.etat_lecteur().preset_count, Some(23));
    // Some(0) écrase : le cd sans disque dit « rien à numéroter ».
    core.handle_source_update("radio", update_avec_compte(Some(0))).await;
    assert_eq!(core.etat_lecteur().preset_count, Some(0));
}

#[tokio::test]
async fn le_compte_survit_a_larret_mais_pas_au_changement_de_source() {
    // Stop efface preset (plus rien ne joue) mais pas le compte : une radio
    // arrêtée a toujours ses stations.
    // [compte à 23 comme ci-dessus, puis :]
    core.handle_command(Command::Stop).await.unwrap();
    assert_eq!(core.etat_lecteur().preset_count, Some(23));
    core.handle_command(Command::SourceCycle).await.unwrap();
    assert_eq!(core.etat_lecteur().preset_count, None);
}

#[tokio::test]
async fn le_compte_est_oublie_en_veille() {
    // [compte à 23, puis :]
    core.handle_command(Command::Power).await.unwrap(); // entre en veille
    assert_eq!(core.etat_lecteur().preset_count, None);
}
```

`update_avec_compte` is a sketch: build the `SourceUpdate` literal the way neighboring tests do (all fields written out, `preset_count: Some(..)`/`None`). If `handle_source_update` has a different call shape in tests, mirror the existing `preset` tests exactly.

- [ ] **Step 2: Run to verify failure** (`cargo test -p ritornello-core` via WSL). Expected: FAIL (no such field).

- [ ] **Step 3: Implement.**
  - `Core` field, right after `preset: Option<u8>` (core.rs:105):

```rust
    /// How many numbered presets the active source offers (stations,
    /// tracks), as last declared. Forgotten on source change and standby —
    /// the next source re-declares it on activate/wake — but kept on stop:
    /// a stopped radio still has its stations.
    preset_count: Option<u8>,
```

    Initialize `preset_count: None` in the constructor.
  - In `handle_source_update`, right after the `preset` block (core.rs:245-247):

```rust
        if let Some(c) = update.preset_count {
            self.preset_count = Some(c);
        }
```

  - In `appliquer_commande`: add `self.preset_count = None;` in the `Command::SourceCycle` arm (next to its `set_identity(None)`) and in the `Command::Power` arm on the path that ENTERS standby only (read the arm; it toggles — the wake-up path must not clear anything).
  - `PlayerState` (metadata.rs:45-59), after `preset`:

```rust
    /// Nombre de présélections numérotées offertes par la Source active
    /// (stations pour la radio, pistes pour le cd), tel qu'elle l'a déclaré.
    /// `None` = rien déclaré : l'IHM retombe sur la grille 1-9 historique.
    /// `Some(0)` = rien à numéroter (cd sans disque) : aucune touche.
    pub preset_count: Option<u8>,
```

    Copy it in `etat_lecteur()` (core.rs:369-378): `preset_count: self.preset_count,`. Fix any `PlayerState` literal in tests by adding the field.

- [ ] **Step 4: Run tests.** `cargo test -p ritornello-core` via WSL. Expected: PASS. Then `cargo test --workspace` (ripple check).

- [ ] **Step 5: Commit.** `git commit -am "feat(core): mémorise et publie le compte de présélections de la source active"`

---

### Task 3: Core — pending tens offset (`Plus10`, key 0), overlay display

**Files:**
- Modify: `crates/ritornello-core/src/core.rs`
- Modify: `crates/ritornello-core/src/locales/en.toml`
- Modify: `deploy/locales/core/fr.toml`

**Interfaces:**
- Consumes: `Command::Plus10` (Task 1), `Core.preset_count` (Task 2), existing `overlay`/`OVERLAY`/`expire_overlay`/`push_view`.
- Produces: nothing new for later tasks (behavior only). i18n key `preset_label` (en `"PRESET"`, fr `"PRESELECTION"`).

- [ ] **Step 1: Write the failing tests** in `core.rs` tests (same harness as Task 2; `source_calls` is the existing fake-source call log — see the test asserting `Select(3)` around core.rs:1880):

```rust
#[tokio::test]
async fn plus10_saffiche_et_repousse_son_echeance() {
    // Chaque appui montre le cumul (+10, +20) dans l'incrustation, avec la
    // même échéance que le volume.
    core.handle_command(Command::Plus10).await.unwrap();
    assert!(core.overlay_deadline().is_some());
    // [vérifier la ligne 2 de la vue poussée == "+10", comme les tests
    //  d'overlay volume lisent la vue ; puis :]
    core.handle_command(Command::Plus10).await.unwrap();
    // ligne 2 == "+20"
}

#[tokio::test]
async fn le_decalage_est_consomme_par_la_touche_chiffre() {
    // +10 puis 4 = présélection 14 ; le décalage ne survit pas à sa
    // consommation.
    // [compte à 23 déclaré, puis :]
    core.handle_command(Command::Plus10).await.unwrap();
    core.handle_command(Command::Select(4)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(14)")));
    core.handle_command(Command::Select(4)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(4)")));
}

#[tokio::test]
async fn la_touche_zero_seule_ne_fait_rien() {
    core.handle_command(Command::Select(0)).await.unwrap();
    assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Select(0)")));
}

#[tokio::test]
async fn zero_atteint_les_multiples_de_dix() {
    // 20 stations : +10 +10 puis 0 = 20 — le décalage 20 doit rester permis.
    // [compte à 20 déclaré, puis :]
    core.handle_command(Command::Plus10).await.unwrap();
    core.handle_command(Command::Plus10).await.unwrap();
    core.handle_command(Command::Select(0)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(20)")));
}

#[tokio::test]
async fn plus10_reboucle_apres_la_derniere_dizaine() {
    // 23 stations : décalages utiles 10 et 20 ; le troisième appui revient
    // à zéro et éteint l'incrustation, comme la fenêtre web.
    // [compte à 23 déclaré, puis :]
    for _ in 0..3 { core.handle_command(Command::Plus10).await.unwrap(); }
    assert!(core.overlay_deadline().is_none());
    core.handle_command(Command::Select(3)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
}

#[tokio::test]
async fn une_autre_commande_abandonne_le_decalage() {
    core.handle_command(Command::Plus10).await.unwrap();
    core.handle_command(Command::VolumeUp).await.unwrap();
    core.handle_command(Command::Select(3)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
}

#[tokio::test]
async fn lecheance_de_lincrustation_oublie_le_decalage() {
    core.handle_command(Command::Plus10).await.unwrap();
    core.expire_overlay();
    core.handle_command(Command::Select(3)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
}

#[tokio::test]
async fn sans_compte_connu_le_decalage_sature_sans_reboucler() {
    // Pas de compte déclaré : on ne sait pas où est la fin, donc pas de
    // rebouclage — saturation à 240.
    for _ in 0..30 { core.handle_command(Command::Plus10).await.unwrap(); }
    core.handle_command(Command::Select(3)).await.unwrap();
    assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(243)")));
}
```

Adapt harness plumbing (core construction, count declaration via `handle_source_update`) from the neighboring tests; the sequences and assertions above are the requirements.

- [ ] **Step 2: Run to verify failure.** Expected: compile error — `appliquer_commande`'s match is exhaustive and misses `Plus10`.

- [ ] **Step 3: Implement.**
  - Field after `preset_count`:

```rust
    /// Remote tens offset in flight: `Plus10` presses accumulate here until
    /// a digit key consumes them (`+10` then `4` selects 14). It lives and
    /// dies with the overlay that displays it — one deadline, not two
    /// timers.
    pending_tens: u8,
```

    Initialize to `0`.
  - Top of `appliquer_commande`, before the `match`:

```rust
        // Any command other than Plus10/Select abandons a pending tens
        // sequence: pressing volume mid-sequence is a change of mind, not a
        // step of it.
        if !matches!(cmd, Command::Plus10 | Command::Select(_)) {
            self.pending_tens = 0;
        }
```

  - Rewrite the `Select` arm:

```rust
            Command::Select(n) => {
                let tens = std::mem::take(&mut self.pending_tens);
                if tens != 0 {
                    // The consumed offset's overlay has said what it had to
                    // say; the source's own view takes over.
                    self.overlay = None;
                    self.push_view();
                }
                let n = n.saturating_add(tens);
                if n == 0 {
                    // Key 0 with no pending offset: nothing to select.
                    return Ok(());
                }
                self.retry_count = 0;
                let action = self.active().request(SourceReq::Select(n)).await?;
                self.apply(action).await?;
            }
```

  - New `Plus10` arm:

```rust
            Command::Plus10 => {
                let next = self.pending_tens.saturating_add(10);
                self.pending_tens = match self.preset_count {
                    // Wrap past the last useful decade: the largest
                    // reachable multiple of 10 is (count / 10) * 10
                    // (station 20 is +10 +10 then 0, so offset 20 must
                    // stay allowed for a count of 20).
                    Some(count) if next > (count / 10) * 10 => 0,
                    // No known count: saturate, don't wrap — we can't know
                    // where the end is.
                    None => next.min(240),
                    _ => next,
                };
                if self.pending_tens == 0 {
                    self.overlay = None;
                    self.push_view();
                } else {
                    self.show_tens_overlay().await;
                }
            }
```

  - New method next to `show_overlay` (core.rs:781):

```rust
    /// Overlay for the pending tens offset ("+10", "+20"): same slot and
    /// same deadline as the volume overlay, so each press pushes the
    /// deadline back and expiry forgets the offset together with the
    /// display.
    async fn show_tens_overlay(&mut self) {
        let line1 = self.catalog.read().await.get("preset_label").to_string();
        let line2 = format!("+{}", self.pending_tens);
        self.overlay =
            Some((View { line1, line2, line3: String::new() }, Instant::now() + OVERLAY));
        self.push_view();
    }
```

  - Extend `expire_overlay` (core.rs:~800): add `self.pending_tens = 0;` before `push_view()`.
  - i18n: `crates/ritornello-core/src/locales/en.toml` add `preset_label = "PRESET"` (next to `volume_label`); `deploy/locales/core/fr.toml` add `preset_label = "PRESELECTION"`.
  - `held` on `Plus10` needs no code: `handle_input`'s held path only matches `VolumeUp`/`VolumeDown`.

- [ ] **Step 4: Run tests.** `cargo test -p ritornello-core` then `cargo test --workspace` via WSL. Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "feat(core): décalage +10 de la télécommande, touche 0 et incrustation du cumul"`

---

### Task 4: Radio — declare the count, allow presets up to 99

**Files:**
- Modify: `crates/ritornello-plugin-radio/src/config.rs`
- Modify: `crates/ritornello-plugin-radio/src/main.rs`
- Modify: `crates/ritornello-plugin-radio/src/locales/en.toml` (and check `ui` catalog if the message appears there)
- Modify: `deploy/locales/radio/fr.toml`

**Interfaces:**
- Consumes: `SourceOutcome::preset_count(n)` (Task 1).
- Produces: `Stations::preset_count(&self) -> u8`.

- [ ] **Step 1: Write the failing tests.**
  - In `config.rs` tests: the validation bound moves to 1..=99. Find the existing out-of-range test and adjust/extend:

```rust
#[test]
fn la_validation_accepte_les_preselections_jusqua_99() {
    // [construire une table valide comme les tests voisins, avec un
    //  preset = 42 : valide ; preset = 100 : PresetOutOfRange ;
    //  preset = 0 : toujours refusé.]
}

#[test]
fn le_compte_est_la_plus_haute_preselection() {
    // Via l'admin les présélections sont contiguës 1..N (max == len) ; une
    // table éditée à la main avec des trous expose juste des numéros vides,
    // servis par l'éphémère « présélection vide » existant.
    // [table avec presets 1, 5, 9 → preset_count() == 9 ; table vide → 0.]
}
```

  - In `main.rs` tests: find the test exercising `play_preset`/`select` (e.g. the empty-preset transient test at main.rs:339-353) and add assertions that the produced `SourceOutcome.preset_count` is `Some(<max>)` on BOTH branches (hit and empty-preset).

- [ ] **Step 2: Run to verify failure.** `cargo test -p ritornello-plugin-radio` via WSL.

- [ ] **Step 3: Implement.**
  - `config.rs`: change the `1..=9` validation bound (config.rs:89-91) to `1..=99`; update the `ValidationError::PresetOutOfRange` `Display` text and the i18n message texts (`preset_out_of_range` or similar key — grep the locales) from "1-9" to "1-99" in the crate's `en.toml` AND `deploy/locales/radio/fr.toml`. Add:

```rust
    /// Highest preset number in the table — what the web grid shows. Through
    /// the admin presets are contiguous 1..N so this is also the count; a
    /// hand-edited sparse table just exposes a few empty numbers, answered
    /// by the existing "empty preset" transient.
    pub fn preset_count(&self) -> u8 {
        self.stations.iter().map(|s| s.preset).max().unwrap_or(0)
    }
```

    (on the `Stations` impl; adapt the field access to the actual struct.)
  - `main.rs` `play_preset` (lines 52-87): compute `let count = stations.preset_count();` after taking the read lock, then append `.preset_count(count)` to BOTH branches (after `.preset(n)` on the hit branch, after `.transient()` on the empty branch).

- [ ] **Step 4: Run tests.** `cargo test -p ritornello-plugin-radio` via WSL. Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "feat(radio): déclare le compte de stations et accepte les présélections jusqu'à 99"`

---

### Task 5: CD — declare the track count, lift the 1–9 preset cap

**Files:**
- Modify: `crates/ritornello-plugin-cd/src/main.rs`

**Interfaces:**
- Consumes: `SourceOutcome::preset_count(n)` (Task 1).
- Produces: nothing new.

- [ ] **Step 1: Write the failing tests.** Update `la_piste_en_lecture_est_declaree_comme_touche_active` (main.rs:543-560): it currently asserts `preset == None` beyond track 9 — track 12 must now declare `preset: Some(12)`. Add:

```rust
#[test]
fn le_compte_de_pistes_suit_la_toc() {
    // TOC connue → total des pistes ; pas de TOC (pas de disque, ou lecture
    // en cours de la TOC) → 0, « rien à numéroter ».
    // [construire CdSource comme les tests voisins : avec toc et
    //  total_tracks = 12 → issue(..).preset_count == Some(12) ;
    //  sans toc → Some(0).]
}
```

- [ ] **Step 2: Run to verify failure.** `cargo test -p ritornello-plugin-cd` via WSL.

- [ ] **Step 3: Implement.** In `issue()` (main.rs:88-116): before the `match`, declare the count on every frame; in the playing branch, drop the `(1..=9)` guard:

```rust
    fn issue(&self, action: SourceAction) -> SourceOutcome {
        let sortie = SourceOutcome::new(action).with_view(self.view());
        let sortie = sortie.line2_replaceable();
        // The count is a property of the inserted disc, not of playback: it
        // is declared on every frame, 0 when no TOC is known (no disc, or
        // the TOC is still being read).
        let count = match &self.toc {
            Some(_) => u8::try_from(self.total_tracks).unwrap_or(255),
            None => 0,
        };
        let sortie = sortie.preset_count(count);
        match (self.lecture && self.present, &self.toc) {
            (true, Some(toc)) => {
                let sortie = sortie.plays(serde_json::json!({
                    "kind": "disc", "toc": toc, "tracks": self.total_tracks, "track": self.track,
                }));
                match u8::try_from(self.track + 1) {
                    Ok(n) => sortie.preset(n),
                    Err(_) => sortie,
                }
            }
            _ => sortie.plays_nothing(),
        }
    }
```

- [ ] **Step 4: Run tests.** `cargo test -p ritornello-plugin-cd`, then `cargo test --workspace` via WSL. Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "feat(cd): déclare le nombre de pistes et la piste active au-delà de 9"`

---

### Task 6: generic-input — key 0 and +10 assignable, validation 0–9

**Files:**
- Modify: `crates/ritornello-plugin-generic-input/src/bindings.rs`
- Modify: `crates/ritornello-plugin-generic-input/src/locales/en.toml`
- Modify: `deploy/locales/generic-input/fr.toml`
- Modify: `crates/ritornello-plugin-generic-input/ui/src/preset-toml.ts`
- Modify: `crates/ritornello-plugin-generic-input/ui/src/preset-toml.test.ts`
- Modify: `crates/ritornello-plugin-generic-input/ui/src/InputAdmin.test.ts`
- Modify: `web/app/e2e/parcours.spec.ts` (action-row count only)

**Interfaces:**
- Consumes: `Command::Plus10` (Task 1).
- Produces: `ACTIONS` grows to 21 entries — `act_select_0` → `{ cmd: 'Select', arg: 0 }` and `act_plus10` → `{ cmd: 'Plus10' }`, inserted right after `act_select_9`, in that order.

- [ ] **Step 1: Write the failing Rust tests.** In `bindings.rs` tests, adjust `validate_refuse_un_select_hors_bornes`: `Select(0)` is now VALID (move it to a positive assertion), `Select(10)` still rejected. Add:

```rust
#[test]
fn plus10_se_lie_et_fait_le_tour_en_toml() {
    let b = Binding::new(11, &Command::Plus10);
    let t = toml::to_string_pretty(&b).unwrap();
    assert!(t.contains("cmd = \"Plus10\""), "TOML produit: {t}");
    assert!(!t.contains("arg"), "TOML produit: {t}");
    assert_eq!(toml::from_str::<Binding>(&t).unwrap(), b);
    let mut table = Bindings::default();
    table.devices.push(Device { name: "X".into(), bindings: vec![b] });
    assert!(table.validate().is_ok());
}
```

- [ ] **Step 2: Run to verify failure**, then **implement the Rust side**: `validate` bound `(0..=9).contains(&n)`; `Display` text "preset {arg} out of range 0-9 on {device}"; `select_out_of_range` message in `en.toml` ("0-9") and `deploy/locales/generic-input/fr.toml` ("0-9"). Run `cargo test -p ritornello-plugin-generic-input` via WSL: PASS.

- [ ] **Step 3: Write the failing ui tests.** In `preset-toml.test.ts`: `expect(ACTIONS).toHaveLength(21)` and extend the exact command-name list assertion with the two new entries (they sit between `act_select_9` and `act_volume_up`). In `InputAdmin.test.ts:47-51`: `toHaveLength(21)`. Run `npx vitest run` in `crates/ritornello-plugin-generic-input/ui`: FAIL.

- [ ] **Step 4: Implement the ui side.** In `preset-toml.ts`, after the `Array.from({length: 9}, ...)` spread:

```ts
  // La touche 0 et +10 de la télécommande : 0 vaut « décalage + 0 » (10, 20…)
  // et +10 cumule le décalage tenu par le cœur.
  { key: 'act_select_0', cmd: { cmd: 'Select', arg: 0 } },
  { key: 'act_plus10', cmd: { cmd: 'Plus10' } },
```

Update the "Les 19 actions" comment to 21. If the local `Command` TS type doesn't admit `{ cmd: 'Plus10' }`, extend that type the way the other argument-less commands are declared. Add the i18n keys: `en.toml` `act_select_0 = "Key 0"`, `act_plus10 = "+10"`; `deploy/locales/generic-input/fr.toml` `act_select_0 = "Touche 0"`, `act_plus10 = "+10"` (next to the other `act_*` keys — the `i18nKeysUsed` guard pulls `ACTIONS` keys automatically).

- [ ] **Step 5: Update the e2e count.** In `web/app/e2e/parcours.spec.ts:25`: `toHaveCount(19)` → `toHaveCount(21)`. (The e2e suite itself runs in Task 9.)

- [ ] **Step 6: Run the ui tests.** `npx vitest run` in the plugin's `ui/`: PASS (including `i18nKeysUsed`).

- [ ] **Step 7: Commit.** `git commit -am "feat(generic-input): touche 0 et +10 assignables, Select 0-9"`

---

### Task 7: Web — `preset_count` in the payload, shifted-window grid

**Files:**
- Modify: `web/app/src/types.ts`
- Modify: `web/app/src/views/HomeView.vue`
- Modify: `web/app/src/views/HomeView.test.ts`

**Interfaces:**
- Consumes: `PlayerState.preset_count` over SSE (Task 2).
- Produces: `data-preset-plus10` attribute on the +10 button (e2e, Task 9); `PlayerPayload.preset_count: number | null`.

- [ ] **Step 1: Add the payload field.** In `types.ts`, `PlayerPayload` gains `preset_count: number | null` (after `preset`). In `HomeView.test.ts`, add `preset_count: null` to the `complet` default payload object of `FauxEventSource` — the fallback keeps every existing test meaningful.

- [ ] **Step 2: Write the failing tests** (in `HomeView.test.ts`; use `vi.useFakeTimers()` — mirror how other fake-timer tests in the repo set up and always restore real timers after). The existing `expose les 9 présélections de la télécommande` test stays as the fallback test (payload has `preset_count: null`) — rename it `sans compte déclaré, la grille retombe sur 1-9` and additionally assert `w.find('[data-preset-plus10]').exists()` is false. New tests:

```ts
it('la grille ne montre que les numéros existants', async () => {
  const { w } = await monterAvec({ preset_count: 5 }) // adapter au helper local
  expect(w.findAll('[data-preset-button]')).toHaveLength(5)
  expect(w.find('[data-preset-plus10]').exists()).toBe(false)
})

it('un compte nul ne montre aucune touche numérotée', async () => {
  const { w } = await monterAvec({ preset_count: 0 })
  expect(w.findAll('[data-preset-button]')).toHaveLength(0)
  expect(w.find('[data-preset-plus10]').exists()).toBe(false)
})

it('+10 décale la fenêtre, puis reboucle sur la première', async () => {
  const { w } = await monterAvec({ preset_count: 23 })
  expect(w.findAll('[data-preset-button]')).toHaveLength(9) // 1-9
  await w.find('[data-preset-plus10]').trigger('click')
  let nums = w.findAll('[data-preset-button]').map((b) => b.text())
  expect(nums).toEqual(['10', '11', '12', '13', '14', '15', '16', '17', '18', '19'])
  await w.find('[data-preset-plus10]').trigger('click')
  nums = w.findAll('[data-preset-button]').map((b) => b.text())
  expect(nums).toEqual(['20', '21', '22', '23'])
  await w.find('[data-preset-plus10]').trigger('click')
  expect(w.findAll('[data-preset-button]')).toHaveLength(9) // retour 1-9
})

it('la fenêtre retombe seule après 2 s, comme l\'incrustation du cœur', async () => {
  const { w } = await monterAvec({ preset_count: 23 })
  await w.find('[data-preset-plus10]').trigger('click')
  vi.advanceTimersByTime(2000)
  await nextTick()
  expect(w.findAll('[data-preset-button]')).toHaveLength(9)
})

it('choisir un numéro poste la valeur absolue et referme la fenêtre', async () => {
  const { w } = await monterAvec({ preset_count: 23 })
  await w.find('[data-preset-plus10]').trigger('click')
  await w.find('[data-preset-button="14"]').trigger('click')
  // [asserter le POST /api/command {cmd: 'Select', arg: 14} comme le test
  //  Select existant]
  expect(w.findAll('[data-preset-button]')).toHaveLength(9)
})

it('met en évidence la touche active au-delà de 9', async () => {
  const { w } = await monterAvec({ preset_count: 23, preset: 14 })
  await w.find('[data-preset-plus10]').trigger('click')
  expect(w.find('[data-preset-button="14"]').attributes('data-preset-active')).toBe('true')
})
```

`monterAvec` is a sketch for "mount and push a payload with these overrides through `FauxEventSource`" — reuse the file's existing mounting/pushing helpers verbatim.

- [ ] **Step 3: Run to verify failure.** `npx vitest run src/views/HomeView.test.ts` in `web/app`.

- [ ] **Step 4: Implement the grid.** In `HomeView.vue`, replace the `PRESETS` constant with the windowed model (comments in French, as the file does):

```ts
// Même valeur que la constante OVERLAY du cœur (2 s) : la fenêtre web et le
// décalage +10 de la télécommande retombent au même rythme.
const FENETRE_MS = 2000
const fenetre = ref(0)
let minuterieFenetre: ReturnType<typeof setTimeout> | undefined

// Compte déclaré par la source (null = source muette sur le sujet : grille
// 1-9 historique, pour ne jamais désarmer la télécommande).
const compte = computed(() => etat.value?.preset_count ?? null)

// Numéros de la fenêtre courante, seulement ceux qui existent. Fenêtre 0 :
// 1-9 (les touches nues de la télécommande) ; fenêtre k : 10k à 10k+9 (le
// 0 de la télécommande donne 10k).
const presets = computed(() => {
  const c = compte.value
  if (c === null) return Array.from({ length: 9 }, (_, i) => i + 1)
  const debut = fenetre.value === 0 ? 1 : fenetre.value * 10
  const fin = Math.min(fenetre.value * 10 + 9, c)
  return debut > fin ? [] : Array.from({ length: fin - debut + 1 }, (_, i) => debut + i)
})

const plus10Visible = computed(() => (compte.value ?? 0) > 9)

function armerRetour() {
  clearTimeout(minuterieFenetre)
  minuterieFenetre = setTimeout(() => { fenetre.value = 0 }, FENETRE_MS)
}

function decaler() {
  // Même règle que le cœur : la dernière fenêtre utile commence au plus
  // grand multiple de 10 encore atteignable ((compte / 10) * 10).
  const c = compte.value ?? 0
  fenetre.value = (fenetre.value + 1) * 10 > c ? 0 : fenetre.value + 1
  armerRetour()
}

function choisir(n: number) {
  // Le web envoie toujours le numéro absolu : Plus10 ne voyage jamais
  // depuis la SPA, la fenêtre est un état purement local.
  clearTimeout(minuterieFenetre)
  fenetre.value = 0
  send({ cmd: 'Select', arg: n })
}

// Un changement de compte (autre source, disque éjecté) invalide la fenêtre.
watch(compte, () => { fenetre.value = 0 })
onUnmounted(() => clearTimeout(minuterieFenetre))
```

Template — keep every existing attribute, switch the loop to `presets`, the click to `choisir(n)`, and append the +10 cell:

```html
<div :class="['grid grid-cols-3 gap-2',
              presets.length + (plus10Visible ? 1 : 0) > 9 ? 'sm:grid-cols-10' : 'sm:grid-cols-9']">
  <Button
    v-for="n in presets" :key="n"
    :data-preset-button="n"
    :data-preset-active="etat?.preset === n ? 'true' : undefined"
    :aria-current="etat?.preset === n ? 'true' : undefined"
    :variant="etat?.preset === n ? 'default' : 'secondary'"
    @click="choisir(n)"
  >{{ n }}</Button>
  <Button v-if="plus10Visible" data-preset-plus10 variant="secondary" @click="decaler">+10</Button>
</div>
```

Import `watch`, `onUnmounted` from vue if not already there. The `REMOTE_ROWS`/`REMOTE_COMMANDS` blocks are untouched.

- [ ] **Step 5: Run the web suite.** `npx vitest run` in `web/app` (all files — the `i18nKeysUsed` guard must stay green; the +10 label is the literal "+10", no i18n key), plus `npx vue-tsc --noEmit` if that's the repo's typecheck command (check `package.json` scripts). Expected: PASS.

- [ ] **Step 6: Commit.** `git commit -am "feat(web): grille de présélections aux numéros réels avec fenêtre +10"`

---

### Task 8: Web — disable audio "Change" when the GET failed (rider)

**Files:**
- Modify: `web/app/src/views/ConfigView.vue`
- Modify: `web/app/src/views/ConfigView.test.ts`

**Interfaces:** none (self-contained).

- [ ] **Step 1: Write the failing test** (the `monter` helper turns `undefined` table values into 404s — see `un /api/settings injoignable laisse les valeurs par défaut` at ConfigView.test.ts:360):

```ts
it('un /api/audio-output injoignable désactive « Changer »', async () => {
  // Sans cela, le sélecteur affiche « Par défaut (système) » comme si
  // c'était l'état réel, et « Changer » enverrait device: null — une
  // réinitialisation silencieuse.
  const { w } = await monter({ '/api/audio-output': undefined })
  expect(w.find('[data-audio-change]').attributes('disabled')).toBeDefined()
})
```

- [ ] **Step 2: Run to verify failure.** `npx vitest run src/views/ConfigView.test.ts` in `web/app`.

- [ ] **Step 3: Implement.** In `ConfigView.vue`:

```ts
// GET audio en échec : on ne sait pas ce qui est réellement configuré, et
// « Changer » enverrait device: null (réinitialisation silencieuse). On
// désactive le bouton plutôt que de mentir.
const audioIndisponible = ref(false)
```

In `chargerTout()`, replace the audio line:

```ts
  audioIndisponible.value = false
  audio.value = await api.get<AudioPayload>('/api/audio-output').catch(() => {
    audioIndisponible.value = true
    return audio.value
  })
```

Template: `<Button data-audio-change :disabled="audioIndisponible" @click="changerSortie">{{ t('change') }}</Button>`.

- [ ] **Step 4: Run the file's tests** (the nominal "change" tests must still pass — the button is enabled when the GET succeeds). Expected: PASS.

- [ ] **Step 5: Commit.** `git commit -am "fix(web): désactive « Changer » (audio) quand l'état n'a pas pu être lu"`

---

### Task 9: Docs, e2e adjustments, full validation

**Files:**
- Modify: the protocol/architecture doc under `docs/` that describes `SourceMessage` fields and the `Command` list (find it: `grep -l "line2_replaceable" docs/` or `grep -l "Select" docs/*.md`)
- Modify: `web/app/e2e/parcours.spec.ts`

**Interfaces:** consumes everything.

- [ ] **Step 1: Update the docs** (English): add `preset_count` to the SourceMessage field table/description (semantics: absent = says nothing; `Some(0)` = nothing to number; forgotten on source change and standby, kept on stop); add `Plus10` and the legalized `Select(0)` to the command list with the core's tens-offset behavior (cumulative, overlay `+NN` with the volume overlay's deadline, wrap past `(count / 10) * 10`, consumed by the next digit, abandoned by any other command); note the web grid's shifted window mirroring it. Keep the existing document's structure and tone.

- [ ] **Step 2: Adjust the e2e preset assertions.** In `parcours.spec.ts`: the harness radio has exactly ONE station, so once the SPA receives `preset_count: 1` the home grid shows a single digit button. Near the existing line 14 assertion, assert the new reality:

```ts
await expect(page.locator('[data-preset-button="1"]')).toBeVisible()
await expect(page.locator('[data-preset-button]')).toHaveCount(1)
await expect(page.locator('[data-preset-plus10]')).toHaveCount(0)
```

(Keep the `data-preset-active` assertions at lines 88-89 — still valid. Do not touch the theme-picker/radio-admin `[data-preset]` locators — different attribute.)

- [ ] **Step 3: Full validation, in this exact order:**
  1. `npm run build` in `web/app` (SPA dist refresh).
  2. `npx vitest run` in `web/app`, in `web/kit` if it has tests, and in each plugin `ui/` (generic-input, radio) — all green.
  3. `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/preselections && touch crates/ritornello-core/build.rs && cargo build --workspace && cargo test --workspace"` — green.
  4. `wsl.exe -- bash -lc "... && cargo clippy --workspace --all-targets -- -D warnings"` — clean.
  5. `npx playwright test` in `web/app` — all e2e green (the harness spawns the freshly built core + plugins; if it fails on a stale SPA, redo 1 then 3).

- [ ] **Step 4: Commit.** `git commit -am "docs+e2e: protocole preset_count/Plus10 et grille aux numéros réels"`

---

## Self-review notes (already applied)

- Spec coverage: preset_count protocol (T1), core memory/SSE (T2), tens offset + overlay + i18n (T3), radio (T4), cd (T5), generic-input + admin actions (T6), web grid (T7), audio rider (T8), docs/e2e/validation (T9). The accepted web-vs-remote race is documented in the spec, no task needed.
- Type consistency: `preset_count: Option<u8>` / `number | null`; builder `.preset_count(n: u8)`; `data-preset-plus10`; `FENETRE_MS = 2000`; wrap rule `(count / 10) * 10` used identically in T3 (core) and T7 (web `decaler`).
- Task 1 deliberately includes the SDK + core-test ripple so every commit leaves the workspace green.
