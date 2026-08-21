# Continuous metrics history and filterable error list — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the System tab's history graph keep collecting while the browser
tab is hidden and while the user is on another SPA page, add temperature as a
third curve, and give the error list a filterable dialog backed by a much
larger buffer.

**Architecture:** The polling loop and its history move out of `SystemView.vue`
into a module-scoped store, `web/app/src/composables/useMetriques.ts` — the same
shape as `useCatalog.ts`, whose `catalog` ref lives at module scope and is
shared by every view. `App.vue` starts it once at SPA mount, so history exists
before the user ever opens `/system`, survives route changes, and no longer
stops on `visibilitychange`. The view becomes rendering plus power actions. On
the error side, the core's `LogBuffer` grows from 50 to 500 lines and the card
shows only the most recent few, with a button opening a `Dialog` that lists
everything behind a substring filter — the filter itself is a pure function in
its own module, tested directly, mirroring `filterPresets` + `ThemePicker.vue`.

**Tech Stack:** Vue 3 + TypeScript (`@ritornello/ui`, vue-router), vitest,
Rust (axum, tokio).

**Design:** validated in conversation; restated under "Design decisions" below.
No separate spec file — this is a bounded change to a flow that already exists.

## Global Constraints

- **Working directory:** `C:\projets\perso\ritornello\.claude\worktrees\metriques-continues`
  (git worktree, branch `worktree-metriques-continues`). Never `cd` to the
  shared checkout at `C:\projets\perso\ritornello`.
- **Web tests: `npx vitest run` from `web/app`**, never from the worktree root.
  From the root, vitest misses `vitest.config.ts`, rakes 41 files instead of 17
  and fails en masse without the Vue plugin.
- **`node_modules` junctions are already in place** in this worktree
  (`web/app/node_modules/vue-router` → main checkout, and
  `node_modules/@ritornello/ui` → this worktree's `web/kit`). If missing,
  recreate with `New-Item -ItemType Junction`. **Never** create one for `vite`:
  two instances coexist, `@vitejs/plugin-vue` registers on the wrong one, and
  every `.vue` file reports "invalid JS syntax".
- **Rust runs under WSL only**, from PowerShell:
  `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/metriques-continues && cargo test -p ritornello-core"`.
  A bare `cargo` does not exist on the Windows side of this workshop. Same form
  for `cargo clippy -p ritornello-core --all-targets -- -D warnings`.
- **Baseline is green at plan time:** web 208 tests / 17 files; Rust
  `-p ritornello-core` 273 tests. Any red beyond what a task's own step
  predicts is a regression, not an accepted cost.
- **Comment language is French; log/`tracing` message language is English.**
  A test refuses a raw i18n key reaching the screen
  (`web/app/src/i18nKeysUsed.test.ts`).
- **Every new i18n key goes in BOTH catalogs, in the same commit as the
  template that uses it:** `crates/ritornello-core/src/locales/en.toml` (the
  embedded English fallback) and `deploy/locales/core/fr.toml`.
- **Placeholder syntax in catalogs is `{name}`** (see `createT` in
  `web/kit/src/i18n.ts`), passed as `t('key', { name: value })`.
- **TDD is mandatory:** write the failing test, run it and watch it fail for
  the stated reason, implement, watch it pass, commit.

## Design decisions (do not undo these while implementing)

1. **Module-scoped store, exactly one boot point.** `useMetriques()` returns
   refs over module-level state. `App.vue` calls `demarrer()` once;
   `SystemView.vue` starts nothing. Two boot points would race for one timer.
2. **Hidden tabs keep polling.** The `document.hidden` guard and the
   `visibilitychange` listener disappear. Browsers throttle timers in hidden
   tabs (>= 1 s, and roughly one tick per minute after a few minutes), so
   samples taken while away are sparse — accepted, not a bug. The x-axis is
   already timestamp-based (`abscisses` in `views/sparkline.ts`) and the CPU
   delta stays correct because `/proc/stat` jiffies are cumulative, so a
   one-minute gap yields a one-minute average rather than a wrong number.
3. **Unmounting the view no longer stops polling.** Only power actions suspend
   it, via `suspendre()` / `reprendre()`, preserving the single-guard invariant
   that today's `demarrer()` documents.
4. **Every exit path of a power action must resume.** The poll is now shared by
   the whole SPA: leaving it suspended freezes the graph for every page, not
   only the one that suspended it. Poweroff and reboot are the deliberate
   exception — the device is going away, so they stay suspended.
5. **Temperature shares the 0-100 axis.** Degrees Celsius on a Pi live inside
   0-100 (throttling at 80-85), so no second scale is needed and half-height
   reads as 50 degrees. `cheminSparkline` already clamps to 0-100. The legend
   carries the unit, which is what keeps a mixed axis honest.
6. **The temperature curve is distinguished by colour alone** (`text-destructive`,
   the only hue guaranteed distinct from `primary` and `muted-foreground`
   across the 42 presets). Solid stroke, same width as the other two — no dash
   pattern; that was considered and rejected.
7. **A missing temperature erases the whole curve rather than patching over a
   hole.** The three curves, the hover line and the popin share one set of
   abscissae (`abscissesGraphe`); a shorter series would drift from the others.
8. **Logs stay out of the polling loop.** `sonder()` holds an in-flight lock and
   computes a CPU delta between consecutive responses; grafting a second
   request onto it lengthens the lock and changes the observed cadence — that
   is measured, it broke four cadence tests once. The dialog refetches on open,
   on a user gesture, which touches neither.

---

### Task 1: Pure substring filter for journal lines

**Files:**
- Create: `web/app/src/views/journal.ts`
- Test: `web/app/src/views/journal.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `export function filtreLignes(lignes: string[], requete: string): string[]`

`views/journal.ts`, not a composable: a pure function over data, same placement
as `views/sparkline.ts`.

- [ ] **Step 1: Write the failing test**

Create `web/app/src/views/journal.test.ts`:

```ts
import { describe, expect, it } from 'vitest'
import { filtreLignes } from './journal'

const LIGNES = [
  'WARN plugin radio indisponible',
  'ERROR mpv socket closed',
  'WARN CIFS mount timed out',
]

describe('filtreLignes', () => {
  it('rend tout quand la requête est vide', () => {
    expect(filtreLignes(LIGNES, '')).toEqual(LIGNES)
  })

  it('ignore les espaces autour de la requête', () => {
    expect(filtreLignes(LIGNES, '   ')).toEqual(LIGNES)
    expect(filtreLignes(LIGNES, '  mpv  ')).toEqual(['ERROR mpv socket closed'])
  })

  it('filtre par sous-chaîne insensible à la casse', () => {
    expect(filtreLignes(LIGNES, 'WARN')).toEqual([LIGNES[0], LIGNES[2]])
    expect(filtreLignes(LIGNES, 'warn')).toEqual([LIGNES[0], LIGNES[2]])
    expect(filtreLignes(LIGNES, 'CiFs')).toEqual(['WARN CIFS mount timed out'])
  })

  it('rend un tableau vide sans correspondance', () => {
    expect(filtreLignes(LIGNES, 'zzz')).toEqual([])
  })

  it('préserve l ordre reçu', () => {
    // `/api/logs` rend déjà les plus récentes en premier : le filtre ne doit
    // pas retrier, sous peine de renverser cette chronologie.
    expect(filtreLignes(LIGNES, 'o')).toEqual([LIGNES[0], LIGNES[1], LIGNES[2]])
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run, from `web/app`: `npx vitest run src/views/journal.test.ts`
Expected: FAIL — `Failed to resolve import "./journal"`.

- [ ] **Step 3: Write minimal implementation**

Create `web/app/src/views/journal.ts`:

```ts
/**
 * Lignes de journal retenues par une requête de filtre : sous-chaîne,
 * insensible à la casse, ordre d'entrée préservé.
 *
 * L'ordre est un contrat, pas un hasard : `/api/logs` rend les lignes les plus
 * récentes en premier, et un filtre qui retrierait renverserait cette
 * chronologie sans que l'appelant l'ait demandé.
 *
 * Une requête vide — ou faite d'espaces seuls, ce qu'un champ de saisie produit
 * en permanence — rend la liste entière plutôt qu'aucune ligne : un champ qu'on
 * vient de vider doit rendre ce qu'on voyait avant d'y taper.
 */
export function filtreLignes(lignes: string[], requete: string): string[] {
  const q = requete.trim().toLowerCase()
  if (!q) return lignes
  return lignes.filter((l) => l.toLowerCase().includes(q))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run, from `web/app`: `npx vitest run src/views/journal.test.ts`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add web/app/src/views/journal.ts web/app/src/views/journal.test.ts
git commit -m "feat(web): filtre pur des lignes de journal"
```

---

### Task 2: Core keeps 500 error lines instead of 50

**Files:**
- Modify: `crates/ritornello-core/src/main.rs:44`
- Test: `crates/ritornello-core/src/status.rs` (append inside the existing `mod tests`)

**Interfaces:**
- Consumes: existing `LogBuffer::new(capacity)`, `LogBuffer::push`,
  `LogBuffer::snapshot` (`crates/ritornello-core/src/status.rs`, around lines
  477-497).
- Produces: no new API. `GET /api/logs` keeps its shape (`{ lines: string[] }`,
  most recent first — `logs_json` reverses the snapshot).

Only the `main.rs` construction site changes. The sites inside `status.rs`
(`LogBuffer::new(50)`, `LogBuffer::new(10)`) and `admin.rs:189` keep their
sizes: they pin buffer behaviour at a size convenient to assert, not the
production capacity.

- [ ] **Step 1: Write the test**

Append to `mod tests` in `crates/ritornello-core/src/status.rs`, next to the
existing `LogBuffer` tests:

```rust
    /// La capacité de production, pas celle d'un montage de test : le tampon
    /// doit retenir 500 lignes et jeter les plus anciennes, sinon la popin
    /// « toutes les erreurs » de l'IHM n'a rien de plus à montrer que la carte
    /// qui en affiche déjà les dernières.
    #[test]
    fn log_buffer_retient_cinq_cents_lignes() {
        let buf = LogBuffer::new(500);
        for i in 0..600 {
            buf.push(format!("line {i}"));
        }
        let lines = buf.snapshot();
        assert_eq!(lines.len(), 500);
        assert_eq!(lines.first().map(String::as_str), Some("line 100"));
        assert_eq!(lines.last().map(String::as_str), Some("line 599"));
    }
```

- [ ] **Step 2: Run it**

Run: `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/metriques-continues && cargo test -p ritornello-core log_buffer_retient"`

Expected: **PASS immediately.** `LogBuffer` is already generic over its
capacity, so this test states the contract rather than driving new code. Do not
manufacture a failure to satisfy the TDD ritual — say so in the commit and move
on. The actual behaviour change is a constant in `main`, which no unit test can
observe (it is wiring); it is verified by reading the diff in Step 3.

- [ ] **Step 3: Raise the production capacity**

In `crates/ritornello-core/src/main.rs:44`, replace:

```rust
    let log_buffer = Arc::new(LogBuffer::new(50));
```

with:

```rust
    // 500 et non 50 : l'IHM a désormais une popin qui liste tout le tampon
    // derrière un filtre, et 50 lignes ne remontent pas plus loin que la carte
    // qui en affiche déjà les dernières. 500 lignes pèsent quelques dizaines de
    // kio, relevées une fois par ouverture de popin — pas à chaque sondage.
    let log_buffer = Arc::new(LogBuffer::new(500));
```

- [ ] **Step 4: Run the full core suite and clippy**

Run: `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/metriques-continues && cargo test -p ritornello-core"`
Expected: PASS, 274 tests (273 baseline + 1).

Run: `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/metriques-continues && cargo clippy -p ritornello-core --all-targets -- -D warnings"`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/ritornello-core/src/main.rs crates/ritornello-core/src/status.rs
git commit -m "feat(core): tampon de 500 lignes de journal pour la popin d erreurs"
```

---

### Task 3: Truncated error card, button, filterable dialog

**Files:**
- Modify: `web/app/src/views/SystemView.vue`
- Modify: `crates/ritornello-core/src/locales/en.toml`
- Modify: `deploy/locales/core/fr.toml`
- Test: `web/app/src/views/SystemView.test.ts`

**Interfaces:**
- Consumes: `filtreLignes` from Task 1 (`../views/journal` → inside
  `views/`, the import is `./journal`); existing `logs` ref, `LogsPayload`
  from `../types`, `Input` and `Dialog*` from `@ritornello/ui`.
- Produces: DOM contract for later tasks and for Playwright:
  `[data-logs-card]` (unchanged), `[data-log-line]` (card lines, now capped),
  `[data-logs-all]` (button), `[data-logs-filter]` (dialog input),
  `[data-logs-dialog-line]` (dialog lines), `[data-logs-count]`,
  `[data-logs-empty]`.

`[data-logs-dialog-line]` must differ from `[data-log-line]`: the card and the
dialog render at the same time, the dialog into a portal in `document.body`, and
a shared selector would count both.

- [ ] **Step 1: Write the failing tests**

Add a `describe` block to `web/app/src/views/SystemView.test.ts`. The existing
`stub()` helper already takes the journal payload as its third argument, and
`monter()` loads the catalogue before mounting — reuse both.

```ts
  describe('popin des erreurs', () => {
    /** Douze lignes : plus que les huit de la carte, assez pour que le filtre
     *  ait quelque chose à écarter. */
    const DOUZE = Array.from({ length: 12 }, (_, i) =>
      i === 3 ? 'ERROR mpv socket closed' : `WARN ligne ${i}`,
    )

    it('la carte ne montre que les huit erreurs les plus récentes', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      expect(w.findAll('[data-log-line]')).toHaveLength(8)
      expect(w.findAll('[data-log-line]')[0]!.text()).toBe(DOUZE[0])
      w.unmount()
    })

    it('le bouton annonce le total et n apparaît qu au-delà de la carte', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      expect(w.get('[data-logs-all]').text()).toContain('12')
      w.unmount()

      // Trois erreurs : la carte les montre déjà toutes, une popin n'aurait
      // rien de plus à dire.
      stub(payload(), CATALOGUE, { lines: DOUZE.slice(0, 3) })
      const peu = await monter()
      expect(peu.find('[data-logs-all]').exists()).toBe(false)
      peu.unmount()
    })

    it('la popin liste tout le journal', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // La popin est rendue dans un portail : elle vit dans document.body.
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('12 / 12')
      w.unmount()
    })

    it('le champ filtre la liste et met à jour le compteur', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const champ = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      champ.value = 'mpv'
      champ.dispatchEvent(new Event('input'))
      await flushPromises()
      const lignes = document.body.querySelectorAll('[data-logs-dialog-line]')
      expect(lignes).toHaveLength(1)
      expect(lignes[0]!.textContent).toBe('ERROR mpv socket closed')
      expect(document.body.querySelector('[data-logs-count]')!.textContent).toContain('1 / 12')
      w.unmount()
    })

    it('annonce l absence de correspondance', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const champ = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      champ.value = 'zzz'
      champ.dispatchEvent(new Event('input'))
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(0)
      expect(document.body.querySelector('[data-logs-empty]')).not.toBeNull()
      w.unmount()
    })

    it('relève le journal à l ouverture', async () => {
      const f = stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      const avant = f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      // Une requête de plus, sur geste utilisateur : le journal reste hors du
      // sondage périodique (verrou « en vol » et delta CPU de `sonder`).
      expect(f.mock.calls.filter((c) => String(c[0]).includes('/api/logs')).length).toBe(avant + 1)
      w.unmount()
    })

    it('rouvre sans le filtre précédent', async () => {
      stub(payload(), CATALOGUE, { lines: DOUZE })
      const w = await monter()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      const champ = document.body.querySelector<HTMLInputElement>('[data-logs-filter]')!
      champ.value = 'mpv'
      champ.dispatchEvent(new Event('input'))
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(1)

      // Fermeture par le bouton du dialogue — le vrai geste, et le seul
      // `[data-slot="dialog-close"]` présent puisque seul le dialogue ouvert
      // est rendu dans le portail. Puis réouverture : le champ repart vide,
      // sinon la popin s'ouvrirait sur une liste tronquée sans que rien à
      // l'écran ne l'explique.
      document.body.querySelector<HTMLElement>('[data-slot="dialog-close"]')!.click()
      await flushPromises()
      await w.get('[data-logs-all]').trigger('click')
      await flushPromises()
      expect(document.body.querySelectorAll('[data-logs-dialog-line]')).toHaveLength(12)
      w.unmount()
    })
  })
```

Do **not** add `defineExpose` to the view to make any of these tests reach into
its internals; every assertion above goes through the DOM or a user gesture.

Note on the filter input: `Input` (`web/kit/src/components/ui/input/Input.vue`)
is a single-root `<input>` with `v-model` via `useVModel(..., passive: true)`
and no `inheritAttrs: false`, so `data-logs-filter` and `placeholder` land on
the native element and a native `input` event drives the model. If that proves
flaky, `w.findComponent(Input).vm.$emit('update:modelValue', 'mpv')` works even
through the portal, because Vue Test Utils walks the component tree, not the DOM.

- [ ] **Step 2: Run tests to verify they fail**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts -t "popin des erreurs"`
Expected: FAIL — `[data-logs-all]` not found (the card currently renders all
lines and has no button).

- [ ] **Step 3: Add the i18n keys**

In `crates/ritornello-core/src/locales/en.toml`, next to the other `system_*`
keys:

```toml
system_errors_all = "All errors ({count})"
system_errors_title = "All logged errors"
system_errors_filter = "filter"
system_errors_none = "No matching line"
```

In `deploy/locales/core/fr.toml`, at the matching position:

```toml
system_errors_all = "Toutes les erreurs ({count})"
system_errors_title = "Toutes les erreurs journalisées"
system_errors_filter = "filtrer"
system_errors_none = "Aucune ligne correspondante"
```

The English placeholder is lower-case `filter` to match the existing
`theme_filter` convention, which a Playwright journey targets by placeholder
text.

- [ ] **Step 4: Implement in `SystemView.vue`**

Add to the imports from `@ritornello/ui`: `Input`. Add a local import:
`import { filtreLignes } from './journal'`.

Replace the `logs` fetch in `onMounted` — currently the inline
`api.get<LogsPayload>('/api/logs').then(...).catch(...)` chain — with
`void releverJournal()`, and add this block next to the `logs` declaration:

```ts
/** Lignes d'erreur montrées directement dans la carte. Au-delà, la popin prend
 *  le relais : le tampon du cœur en garde 500, et les dérouler dans la page
 *  repousserait tout le reste hors de l'écran. */
const LOGS_CARTE = 8
const erreursOuvertes = ref(false)
const requeteErreurs = ref('')
const logsCarte = computed(() => logs.value.slice(0, LOGS_CARTE))
const logsFiltres = computed(() => filtreLignes(logs.value, requeteErreurs.value))

/**
 * Relève le journal : au montage, et à chaque ouverture de la popin.
 *
 * Un geste utilisateur, donc toujours hors du sondage périodique — voir le
 * commentaire de `logs` : `sonder()` tient un verrou « en vol » et calcule un
 * delta CPU entre deux réponses, et y greffer une seconde requête change la
 * cadence observée (mesuré, quatre tests de cadence sont tombés).
 *
 * Son propre `.catch` : un journal indisponible ne doit pas priver
 * l'utilisateur des métriques, ni l'inverse. Un échec laisse la liste
 * précédente en place plutôt que de la vider — même convention que `reload`
 * de `useCatalog`.
 */
async function releverJournal(): Promise<void> {
  const j = await api.get<LogsPayload>('/api/logs').catch(() => null)
  if (j) logs.value = j.lines ?? []
}

function ouvrirErreurs(): void {
  // Filtre remis à zéro : une popin qui s'ouvre montre tout. Garder la requête
  // précédente la ferait rouvrir sur une liste tronquée, et le champ qui
  // l'explique est en haut du dialogue, pas sous les yeux de qui vient de
  // cliquer le bouton.
  requeteErreurs.value = ''
  erreursOuvertes.value = true
  void releverJournal()
}
```

Replace the logs card body (currently `<ul>` over `logs`) with:

```html
    <Card data-logs-card>
      <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
      <CardContent class="space-y-2">
        <ul class="space-y-1 font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in logsCarte" :key="i" data-log-line>{{ l }}</li>
        </ul>
        <!-- Bouton seulement quand la carte ne montre pas déjà tout : avec
             trois erreurs au compteur, une popin n'aurait rien de plus à dire. -->
        <Button
          v-if="logs.length > LOGS_CARTE"
          variant="outline"
          size="sm"
          data-logs-all
          @click="ouvrirErreurs"
        >
          {{ t('system_errors_all', { count: logs.length }) }}
        </Button>
      </CardContent>
    </Card>
```

Add the dialog next to the two existing ones, before the power dialog:

```html
    <!-- Popin des erreurs : `Dialog` du kit, comme l'aide sur la sous-tension
         et le dialogue d'alimentation, et rendue comme elles dans un portail —
         son contenu vit donc dans `document.body`, ce que les tests savent.
         Le compteur tient dans la `DialogDescription` : il décrit bien le
         dialogue, et l'y mettre lui donne au passage son texte d'accessibilité. -->
    <Dialog v-model:open="erreursOuvertes">
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{{ t('system_errors_title') }}</DialogTitle>
          <DialogDescription data-logs-count>
            {{ logsFiltres.length }} / {{ logs.length }}
          </DialogDescription>
        </DialogHeader>
        <Input
          v-model="requeteErreurs"
          data-logs-filter
          :placeholder="t('system_errors_filter')"
        />
        <ul class="max-h-[60vh] space-y-1 overflow-y-auto font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in logsFiltres" :key="i" data-logs-dialog-line>{{ l }}</li>
        </ul>
        <p v-if="!logsFiltres.length" data-logs-empty class="text-sm text-muted-foreground">
          {{ t('system_errors_none') }}
        </p>
      </DialogContent>
    </Dialog>
```

- [ ] **Step 5: Run the new tests, then the whole web suite**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts -t "popin des erreurs"`
Expected: PASS, 7 tests.

Run, from `web/app`: `npx vitest run`
Expected: PASS. Two existing tests may need adjusting because the card no
longer renders every line — check any assertion counting `[data-log-line]`
(there is one around line 1032 asserting most-recent-first order). Fix by
capping the fixture or asserting on the first 8, never by widening
`LOGS_CARTE`.

- [ ] **Step 6: Commit**

```bash
git add web/app/src/views/SystemView.vue web/app/src/views/SystemView.test.ts crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): popin filtrable de toutes les erreurs journalisees"
```

---

### Task 4: Extract the polling store — mechanical move, no behaviour change

**Files:**
- Create: `web/app/src/composables/useMetriques.ts`
- Modify: `web/app/src/views/SystemView.vue`
- Test: `web/app/src/views/SystemView.test.ts` (harness only)

**Interfaces:**
- Consumes: `SystemPayload` from `../types`, `api` from `@ritornello/ui`.
- Produces:

```ts
export interface Echantillon { cpu: number; ram: number; t: number }
export const PERIODES_S: readonly number[]   // [1, 2, 5, 10, 30] — the template iterates it

export function useMetriques(): {
  etat: Ref<SystemPayload | null>
  indisponible: Ref<boolean>
  historique: Ref<Echantillon[]>
  utilisationCpuActuelle: Ref<number | null>
  periodeMs: Ref<number>
  periode: WritableComputedRef<string>
  dureeFenetreMin: ComputedRef<number>
  demarrer: () => void
  suspendre: () => void
  reprendre: () => void
}

/** Remise à zéro complète, pour les tests : l'état vit au niveau module et
 *  fuiterait d'un test au suivant. */
export function reinitialiserMetriques(): void
```

`CAPACITE`, `sonder`, `arreter`, `utilisationCpu`, `pourcentages`,
`sondageEnVol`, `minuteur`, `attente`, `dernierSondage`, `precedentJiffies` and
the new `suspendu` stay **private to the module** — nothing outside needs them,
and exporting `arreter` would let a caller stop the shared poll with no way to
account for it. `CAPACITE` in particular: after this task the view no longer
references it, so delete its declaration there rather than importing it back.

**This task changes no behaviour.** The `document.hidden` guard, the
`visibilitychange` listener and the unmount stop all still work exactly as
today; Task 5 removes them. Keeping the move mechanical is what makes Task 5
reviewable.

- [ ] **Step 1: Create the store by moving code verbatim**

Move these symbols out of `SystemView.vue`'s `<script setup>` into
`web/app/src/composables/useMetriques.ts`, **keeping their doc comments word
for word** — they carry hard-won reasoning (the in-flight lock, the CPU delta,
the deadline arithmetic, the measured window):

`etat`, `indisponible`, `periodeMs`, `PERIODES_S`, `CAPACITE`, `historique`,
`sondageEnVol`, `minuteur`, `attente`, `dernierSondage`, `precedentJiffies`,
`utilisationCpuActuelle`, `utilisationCpu()`, `pourcentages()`, `sonder()`,
`demarrer()`, `arreter()`, `periode` (the writable computed), `dureeFenetreMin`.

Do **not** move: `logs` and everything from Task 3, `RIEN`, `LARGEUR`,
`HAUTEUR`, `HAUTEUR_REPERE`, `etiquettePeriode` (it needs `t`), every display
computed, all hover state, all power-action code, and `monte`.

File shape:

```ts
import { api } from '@ritornello/ui'
import { computed, ref } from 'vue'
import type { SystemPayload } from '../types'

/**
 * Sondage des métriques système et son historique, au niveau **module** — un
 * seul jeu d'état pour toute la SPA, comme le catalogue de `useCatalog`.
 *
 * Ce n'est pas un détail d'implémentation mais la raison d'être du fichier :
 * quand cet état vivait dans `SystemView.vue`, quitter la page pour la
 * configuration et revenir repartait d'un graphe vide, et l'historique ne
 * commençait à se remplir qu'à la première visite. Ici, `App.vue` l'amorce une
 * fois au montage de la SPA et il vit jusqu'à la fermeture de la page.
 *
 * Un seul point d'amorçage, donc : deux appelants qui démarreraient se
 * disputeraient le même minuteur.
 */

export interface Echantillon { cpu: number; ram: number; t: number }

// ... les déclarations déplacées, dans le même ordre que dans la vue ...

export function useMetriques() {
  return {
    etat,
    indisponible,
    historique,
    utilisationCpuActuelle,
    periodeMs,
    periode,
    dureeFenetreMin,
    demarrer,
    suspendre,
    reprendre,
  }
}
```

Changes to the moved code, and nothing else:

1. Type the history as `Echantillon[]` and have `pourcentages()` return
   `Echantillon | null`, instead of repeating the inline `{ cpu; ram; t }`
   shape in three places.
2. `let monte = true` does **not** move. Replace its two uses inside
   `demarrer()` and `sonder()`-adjacent code as follows: `demarrer()` loses the
   `!monte` clause and gains `suspendu` (next point). The `monte` uses inside
   `attendreRetour` stay in the view.
3. Replace the `enCours.value !== null` clause of `demarrer()`'s guard — the
   view's ref is no longer reachable from here — with a module-private flag and
   its two doors:

```ts
/**
 * Sondage suspendu par une action d'alimentation en cours.
 *
 * Remplace le test `enCours !== null` que faisait `demarrer()` quand tout
 * vivait dans la vue : le sondage est désormais partagé par toute la SPA et ne
 * peut plus lire l'état d'une page. La garde reste unique et reste ici — c'est
 * elle qui empêche un retour de visibilité ou un changement de période de
 * rallumer le sondage sur un cœur qu'on vient d'éteindre.
 *
 * Contrepartie à ne pas perdre de vue : laissé à `true`, il fige le graphe pour
 * *toutes* les pages, pas seulement celle qui a suspendu. Tout chemin de sortie
 * d'une action d'alimentation doit appeler `reprendre()` — sauf l'arrêt et le
 * redémarrage de la machine, où l'appareil s'en va pour de bon.
 */
let suspendu = false

export function suspendre(): void {
  suspendu = true
  arreter()
}

export function reprendre(): void {
  suspendu = false
  demarrer()
}
```

   and `demarrer()`'s guard becomes:

```ts
  if (document.hidden || suspendu || minuteur !== null) return
```

4. Add the test-only reset:

```ts
/**
 * Remise à zéro complète. **Pour les tests uniquement** : l'état vit au niveau
 * module, donc sans ça un test laisse son historique, sa période et son
 * minuteur au suivant. À appeler dans un `beforeEach`.
 */
export function reinitialiserMetriques(): void {
  arreter()
  suspendu = false
  dernierSondage = null
  etat.value = null
  indisponible.value = false
  historique.value = []
  periodeMs.value = 5000
  precedentJiffies.value = null
  utilisationCpuActuelle.value = null
}
```

- [ ] **Step 2: Rewire `SystemView.vue`**

At the top of `<script setup>`:

```ts
import { PERIODES_S, useMetriques } from '../composables/useMetriques'

const {
  etat, indisponible, historique, utilisationCpuActuelle,
  periodeMs, periode, dureeFenetreMin, demarrer, suspendre, reprendre,
} = useMetriques()
```

`PERIODES_S` is imported separately because the template iterates it directly.
`LARGEUR`, `HAUTEUR` and `RIEN` stay in the view as they are; delete the view's
`CAPACITE` declaration (it now lives in the store and the view no longer reads it).

`onMounted` keeps `demarrer()` and the `visibilitychange` listener exactly as
today — Task 5 removes them, not this one.

`onUnmounted` is the one place this move cannot be perfectly mechanical. Today
it calls `arreter()`; that function is now module-private on purpose, so the new
body is just:

```ts
onUnmounted(() => {
  monte = false
  document.removeEventListener('visibilitychange', visibilite)
})
```

Unmount therefore stops stopping the poll — the single behaviour change this
task carries. It is the direction Task 5 wants anyway; state it plainly in the
commit message rather than reaching for `arreter()` to postpone it.

Replace `arreter(); demarrer()` in `confirmer()` and `attendreRetour()` with
`suspendre()` and `reprendre()` respectively:

- `confirmer()`: `arreter()` → `suspendre()`; the error branch's `demarrer()` →
  `reprendre()`.
- `attendreRetour()`: both `demarrer()` calls → `reprendre()`.

- [ ] **Step 3: Reset the store between tests**

In `web/app/src/views/SystemView.test.ts`, import the reset and call it, and
start the poll explicitly in `monter()` — the view no longer boots it, `App.vue`
does, so the test harness has to stand in for `App.vue`:

```ts
import { reinitialiserMetriques, useMetriques } from '../composables/useMetriques'
```

```ts
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
    // L'état des métriques vit au niveau module : sans remise à zéro, un test
    // hérite de l'historique, de la période et du minuteur du précédent.
    reinitialiserMetriques()
  })
  afterEach(() => {
    reinitialiserMetriques()
    vi.useRealTimers()
    vi.unstubAllGlobals()
    document.body.innerHTML = ''
  })
```

and in `monter()`, after `await useCatalog().reload()` and before `mount(...)`:

```ts
    // `App.vue` amorce le sondage au montage de la SPA, plus la vue : le
    // harnais de test tient ce rôle, dans le même ordre que l'application.
    useMetriques().demarrer()
```

Apply the same two lines to the one test that mounts `SystemView` directly
instead of through `monter()` (the "l étiquette de période se corrige quand le
catalogue arrive après le montage" test).

- [ ] **Step 4: Run the whole web suite**

Run, from `web/app`: `npx vitest run`
Expected: PASS, same count as after Task 3. A pure move must not change a
single assertion. If a cadence test fails, the move drifted — compare the moved
`demarrer`/`sonder` against git history rather than relaxing the test.

Also run the type check the way the project does — check `web/app/package.json`
for the script name (`npm run typecheck` or `vue-tsc`) and run it. Expected: no
errors.

- [ ] **Step 5: Commit**

```bash
git add web/app/src/composables/useMetriques.ts web/app/src/views/SystemView.vue web/app/src/views/SystemView.test.ts
git commit -m "refactor(web): sort le sondage des metriques de la vue vers un store de module"
```

---

### Task 5: The poll keeps running — hidden tab, and after the view is gone

**Files:**
- Modify: `web/app/src/composables/useMetriques.ts`
- Modify: `web/app/src/views/SystemView.vue`
- Test: `web/app/src/views/SystemView.test.ts`

**Interfaces:**
- Consumes: everything Task 4 produced.
- Produces: no signature change. `demarrer()` loses its `document.hidden`
  clause; `useMetriques()` no longer exposes anything new.

- [ ] **Step 1: Write the failing tests**

Add to `web/app/src/views/SystemView.test.ts`:

```ts
  it('continue de sonder quand l onglet passe en arrière-plan', async () => {
    const f = stub(payload())
    const w = await monter()
    const avant = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    // L'onglet passe en arrière-plan. Le sondage ne doit plus s'arrêter : le
    // graphe est là pour dire ce qui s'est passé pendant qu'on regardait
    // ailleurs.
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(true)
    document.dispatchEvent(new Event('visibilitychange'))
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const apres = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    expect(apres).toBeGreaterThanOrEqual(avant + 3)
    w.unmount()
  })

  it('garde l historique quand on quitte la vue et qu on y revient', async () => {
    const jiffies = prochainsJiffies()
    stub(() => payload(jiffies()))
    const w = await monter()
    // Trois sondages : le premier pose la référence de jiffies, les deux
    // suivants poussent deux échantillons — de quoi tracer une ligne.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const avant = (w.get('[data-system-history] path').attributes('d')!.match(/L/g) ?? []).length
    expect(avant).toBeGreaterThanOrEqual(1)
    w.unmount()

    // La vue est démontée : le sondage continue pour autant, et la vue
    // remontée retrouve un graphe déjà tracé au lieu de repartir de zéro.
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const revenu = mount(SystemView, { attachTo: document.body })
    await flushPromises()
    const apres = (revenu.get('[data-system-history] path').attributes('d')!.match(/L/g) ?? []).length
    expect(apres).toBeGreaterThan(avant)
    revenu.unmount()
  })

  it('quitter la vue pendant un redémarrage de service ne laisse pas le sondage suspendu', async () => {
    // Le sondage est partagé par toute la SPA : le laisser suspendu figerait
    // le graphe de toutes les pages, pas seulement de celle qui a suspendu.
    const f = stub(payload({ service_uptime_s: 3600 }))
    const w = await monter()
    await w.get('[data-power-restart]').trigger('click')
    await flushPromises()
    document.body.querySelector<HTMLElement>('[data-power-confirm]')!.click()
    await flushPromises()
    w.unmount()
    // Le tour de boucle en cours se termine, constate le démontage, et rend la
    // main au sondage normal.
    await vi.advanceTimersByTimeAsync(5000)
    await flushPromises()
    const avant = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    await vi.advanceTimersByTimeAsync(15000)
    await flushPromises()
    const apres = f.mock.calls.filter((c) => String(c[0]).includes('/api/system')).length
    expect(apres).toBeGreaterThanOrEqual(avant + 3)
  })
```

The third test needs `data-power-restart` and the confirm control, both already
present. `payload({ service_uptime_s: 3600 })` keeps the uptime high so the
restart never "succeeds", which is exactly the path being tested.

- [ ] **Step 2: Run tests to verify they fail**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts -t "arrière-plan"`
Expected: FAIL — no further `/api/system` calls after the tab hides.

Run: `npx vitest run src/views/SystemView.test.ts -t "quitter la vue pendant"`
Expected: FAIL — polling stays suspended.

- [ ] **Step 3: Remove the visibility machinery**

In `useMetriques.ts`, `demarrer()`'s guard becomes:

```ts
  // Plus de `document.hidden` ici : le sondage continue en arrière-plan, c'est
  // la raison d'être de ce store. Réserve mesurée et assumée — les navigateurs
  // brident les minuteurs d'un onglet caché (au moins 1 s, et environ un tic
  // par minute au-delà de quelques minutes), donc les échantillons pris pendant
  // une absence sont espacés, pas réguliers. L'axe des abscisses étant tiré des
  // horodatages (`abscisses`, dans `views/sparkline.ts`), le tracé reste juste ;
  // et le delta CPU aussi, les jiffies de `/proc/stat` étant cumulatifs — un
  // trou d'une minute donne une moyenne sur la minute, pas un chiffre faux.
  if (suspendu || minuteur !== null) return
  if (attente !== null) return
```

Rewrite the doc comment on `sonder()` that still claims « Le sondage s'arrête
donc au démontage de la vue et quand l'onglet passe en arrière-plan » — it is
now false. Replace that sentence with:

```
 * Le sondage démarre au chargement de la SPA et vit jusqu'à la fermeture de la
 * page : ni le passage en arrière-plan ni le démontage de la vue ne l'arrêtent,
 * seule une action d'alimentation le suspend. C'est un renversement délibéré de
 * la note d'origine (« ne pas faire travailler un appareil le plus souvent
 * inactif ») : un graphe d'historique qui ne mesure que pendant qu'on le regarde
 * n'apprend rien, et une lecture de `/proc` toutes les 5 s ne coûte rien de
 * mesurable. L'IHM, en pratique, est rarement ouverte.
```

- [ ] **Step 4: Drop the listener and the unmount stop from the view**

In `SystemView.vue`:
- delete the `visibilite()` function;
- `onMounted` keeps only `void releverJournal()` — remove `demarrer()` (Task 6
  moves it to `App.vue`) and the `addEventListener('visibilitychange', ...)`;
- `onUnmounted` keeps `monte = false` and nothing else; delete the
  `removeEventListener` line.

Fix the resumption invariant in `attendreRetour()`. Its tail currently reads
`if (!monte) return` then the timeout toast, `enCours = null`, `demarrer()`.
Replace the tail with:

```ts
  // Sortie par plafond **ou** par démontage : dans les deux cas le sondage doit
  // reprendre. Il est désormais partagé par toute la SPA, et un `return` sec
  // sur `!monte` le laisserait suspendu pour de bon — le graphe de chaque page
  // figé, sans rien à l'écran pour l'expliquer.
  enCours.value = null
  reprendre()
  // Le message d'échec, lui, reste conditionnel : un échec signalé 30 s après
  // que l'utilisateur a quitté la vue n'est que du bruit.
  if (!monte) return
  toast.error(t.value('system_restart_timeout'))
```

Also update the `monte` doc comment, which claims a timer recreated after
unmount « recréerait un minuteur que plus personne ne pourrait jamais
arrêter » — no longer true, the timer is the store's and outlives every view.
Replace with a sentence saying `monte` now governs only the restart-wait loop
and its toast.

- [ ] **Step 5: Run the tests, then the whole suite**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts`
Expected: PASS. Two existing tests must be revisited:
- « ne relance pas le sondage sur un retour de visibilité pendant un arrêt
  confirmé » — still valid and still valuable: it now proves the `suspendu`
  guard alone holds the line. Keep it; if it fails, the guard drifted.
- any test relying on unmount stopping the poll. Fix the test, not the store.

Run, from `web/app`: `npx vitest run`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add web/app/src/composables/useMetriques.ts web/app/src/views/SystemView.vue web/app/src/views/SystemView.test.ts
git commit -m "feat(web): l historique continue onglet cache et vue demontee"
```

---

### Task 6: The SPA boots the poll, not the page

**Files:**
- Modify: `web/app/src/App.vue`
- Test: `web/app/src/App.test.ts`

**Interfaces:**
- Consumes: `useMetriques().demarrer` from Task 4.
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

`web/app/src/App.test.ts` has its own `fetch` stub that answers `/api/i18n`
with the catalogue and **every other URL** with `{ plugins }`. That already
tolerates `/api/system`: the payload carries no jiffies and no memory, so
`utilisationCpu` and `echantillon` both return `null`, nothing is pushed to the
history, and no existing test changes. Only two edits are needed — have `stub()`
return its mock, and reset the module state.

Change the existing `stub()` to return the mock (its body is otherwise
untouched):

```ts
/** `/api/i18n` d'un côté, `/api/status` de l'autre — la nav ne lit rien d'autre.
 *  `/api/system` s'y ajoute depuis que la racine amorce le sondage des
 *  métriques : la charge servie importe peu (aucun jiffy, aucune mémoire, donc
 *  aucun échantillon poussé), seule compte l'existence de l'appel. */
function stub(plugins = [{ name: 'radio', admin: true }]) {
  const f = vi.fn().mockImplementation((url: string) =>
    Promise.resolve({
      ok: true,
      json: async () => (String(url).includes('/api/i18n') ? CATALOGUE : { plugins }),
    } as Response),
  )
  vi.stubGlobal('fetch', f)
  return f
}
```

Add the import and extend `afterEach` — the store's state lives at module
scope, so a leaked `setInterval` would keep firing into the next test:

```ts
import { reinitialiserMetriques } from './composables/useMetriques'
```

```ts
  afterEach(() => {
    reinitialiserMetriques()
    vi.unstubAllGlobals()
  })
```

Then add the test:

```ts
  it('amorce le sondage des métriques au montage de la SPA', async () => {
    // L'historique doit exister avant la première visite de l'onglet système :
    // la vue l'affiche, elle ne le collecte plus. `RouterView` est bouché ici,
    // donc `SystemView` n'est jamais monté — c'est bien la racine qui amorce.
    const f = stub()
    await router.push('/')
    await router.isReady()
    const w = mount(App, { global: { plugins: [router], stubs: { RouterView: true } } })
    await flushPromises()
    expect(f.mock.calls.some((c) => String(c[0]).includes('/api/system'))).toBe(true)
    w.unmount()
  })
```

- [ ] **Step 2: Run test to verify it fails**

Run, from `web/app`: `npx vitest run src/App.test.ts`
Expected: FAIL — no `/api/system` call.

- [ ] **Step 3: Boot from `App.vue`**

Add the import and the call at the very top of `onMounted`, **before**
`await reload()`:

```ts
import { useMetriques } from './composables/useMetriques'
```

```ts
onMounted(async () => {
  // Avant l'`await` du catalogue, et non après : un catalogue lent ne doit pas
  // retarder le premier échantillon. Amorcé ici — la racine de la SPA — et pas
  // dans `SystemView`, pour que l'historique existe avant la première visite de
  // l'onglet système, survive à la navigation, et n'ait qu'un seul point de
  // départ (deux se disputeraient le même minuteur).
  useMetriques().demarrer()
  await reload()
  // ... la suite inchangée
})
```

- [ ] **Step 4: Run the tests**

Run, from `web/app`: `npx vitest run src/App.test.ts`
Expected: PASS, 5 tests (4 baseline + 1).

Run, from `web/app`: `npx vitest run`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/app/src/App.vue web/app/src/App.test.ts
git commit -m "feat(web): la SPA amorce le sondage des metriques a son montage"
```

---

### Task 7: Widen the retained window from 60 to 240 samples

**Files:**
- Modify: `web/app/src/composables/useMetriques.ts`
- Test: `web/app/src/views/SystemView.test.ts` (three existing tests)

**Interfaces:**
- Consumes: `CAPACITE` from Task 4.
- Produces: `CAPACITE === 240`.

Rationale: at the default 5 s period, 60 samples cover 5 minutes. A poll that
now keeps running while the user is away is worth little if it only ever
remembers five minutes. 240 samples cover 20 minutes at 5 s, and up to two
hours at 30 s. Cost: 240 small objects and 240 SVG points, redrawn only on new
samples.

- [ ] **Step 1: Update the three tests that pin the old capacity**

In `web/app/src/views/SystemView.test.ts`:

1. « plafonne l historique à 60 échantillons » → rename to
   « plafonne l historique à 240 échantillons »; advance `241 * 5000` instead
   of `61 * 5000`; expect `239` `L` commands instead of `59`. Update the
   comment's arithmetic to match.
2. « affiche la fenêtre de repli (capacité × période) tant que l historique ne
   mesure rien » → expect `'20 min'` instead of `'5 min'`, and fix the comment
   (`5 s × 240 = 20 min`).
3. « le libellé de la fenêtre suit la période choisie » → replace the two loose
   `toContain` assertions with exact values, which is what makes this test worth
   keeping: `toBe('20 min')` before, `toBe('120 min')` after switching to 30 s.

- [ ] **Step 2: Run to verify they fail**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts -t "plafonne"`
Expected: FAIL — 59 `L` commands where 239 were expected.

- [ ] **Step 3: Raise the constant**

In `useMetriques.ts`:

```ts
/**
 * Nombre d'échantillons conservés — voir `dureeFenetreMin` pour la fenêtre
 * visible qui en découle à la période courante.
 *
 * 240 et non 60 : le sondage tourne désormais en continu, y compris onglet
 * caché et vue démontée, et une mémoire de 5 minutes à la période par défaut
 * ne rendrait presque rien de cette continuité. 240 échantillons font 20
 * minutes à 5 s, deux heures à 30 s. Le plafond est celui de la lisibilité,
 * pas du coût : 240 points sur un graphe large de quelques centaines de pixels
 * sont encore distinguables, quelques milliers ne le seraient plus.
 */
const CAPACITE = 240
```

It stays module-private, as Task 4 left it — no consumer outside the store.

- [ ] **Step 4: Run the whole suite**

Run, from `web/app`: `npx vitest run`
Expected: PASS. The 240-sample test drives 241 polls under fake timers; if it
is slow but green, leave it.

- [ ] **Step 5: Commit**

```bash
git add web/app/src/composables/useMetriques.ts web/app/src/views/SystemView.test.ts
git commit -m "feat(web): fenetre d historique portee a 240 echantillons"
```

---

### Task 8: Temperature as a third curve

**Files:**
- Modify: `web/app/src/composables/useMetriques.ts`
- Modify: `web/app/src/views/SystemView.vue`
- Test: `web/app/src/views/SystemView.test.ts`

**Interfaces:**
- Consumes: `Echantillon` from Task 4, `cheminSparkline` and `abscisses` from
  `views/sparkline.ts` (unchanged — it already clamps to 0-100).
- Produces: `Echantillon` gains `temp: number | null`; `pourcentages()` is
  renamed `echantillon()` since it no longer returns only percentages; DOM
  contract gains `[data-system-history-temp]`.

- [ ] **Step 1: Write the failing tests**

Add to `web/app/src/views/SystemView.test.ts`:

```ts
  describe('courbe de température', () => {
    it('trace la température comme troisième courbe', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      const d = w.get('[data-system-history-temp]').attributes('d')!
      expect(d).not.toBe('')
      // Même échelle que les pourcentages : 47,8 °C se lit à mi-hauteur d'un
      // repère de 30, donc autour de y = 15.
      expect(d).toMatch(/^M[\d.]+,1[0-9]\.\d\d/)
      w.unmount()
    })

    it('ne trace rien sans sonde de température', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: null }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      // Les deux autres courbes restent : une machine sans sonde ne perd pas
      // son graphe.
      expect(w.get('[data-system-history] path').attributes('d')).not.toBe('')
      expect(w.get('[data-system-history-temp]').attributes('d')).toBe('')
      w.unmount()
    })

    it('un trou dans la série efface la courbe entière', async () => {
      // Les trois courbes, le trait de survol et le popin partagent un seul jeu
      // d'abscisses : une série plus courte que les autres dériverait d'elles.
      // Mieux vaut donc une courbe absente qu'une courbe décalée.
      const jiffies = prochainsJiffies()
      let tour = 0
      stub(() => payload({ ...jiffies(), temperature_c: tour++ === 2 ? null : 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(20000)
      await flushPromises()
      expect(w.get('[data-system-history-temp]').attributes('d')).toBe('')
      w.unmount()
    })

    it('annonce la température dans la légende', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      expect(w.get('[data-system-history-legend]').text()).toContain('47.8 °C')
      w.unmount()
    })

    it('n annonce pas de température dans la légende sans sonde', async () => {
      stub(payload({ temperature_c: null }))
      const w = await monter()
      // Pas de série annoncée quand aucune courbe ne peut exister : l'absence
      // de sonde est connue dès le premier sondage, donc rien ne saute.
      expect(w.get('[data-system-history-legend]').text()).not.toContain('°C')
      w.unmount()
    })

    it('affiche la température dans le popin de survol', async () => {
      const jiffies = prochainsJiffies()
      stub(() => payload({ ...jiffies(), temperature_c: 47.8 }))
      const w = await monter()
      await vi.advanceTimersByTimeAsync(15000)
      await flushPromises()
      await w.get('[data-system-history]').trigger('pointermove', { clientX: 10 })
      await flushPromises()
      expect(w.get('[data-system-history-popin]').text()).toContain('47.8 °C')
      w.unmount()
    })
  })
```

Check the existing hover tests for the exact `pointermove` invocation shape
(`clientX`, and whether they stub `getBoundingClientRect`) and copy it — jsdom
reports a zero-width rect unless the existing tests already work around it.

- [ ] **Step 2: Run tests to verify they fail**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts -t "courbe de température"`
Expected: FAIL — `[data-system-history-temp]` not found.

- [ ] **Step 3: Carry temperature in the sample**

In `useMetriques.ts`:

```ts
/**
 * Un point d'historique : deux pourcentages, une température en °C quand la
 * machine en expose une, et l'horodatage du sondage (qui porte l'axe des
 * abscisses, voir `abscisses` dans `views/sparkline.ts`).
 *
 * `temp` est nullable là où `cpu` et `ram` ne le sont pas, et c'est la
 * différence qui compte : une machine sans sonde garde son graphe, alors
 * qu'une machine dont la mémoire ou l'utilisation CPU est illisible n'a pas
 * d'échantillon du tout.
 */
export interface Echantillon { cpu: number; ram: number; temp: number | null; t: number }
```

Rename `pourcentages` to `echantillon` — it no longer returns only percentages —
update its doc comment's first line accordingly, and add the field:

```ts
function echantillon(s: SystemPayload, cpu: number | null): Echantillon | null {
  if (cpu == null || !s.memory || s.memory.total_kb === 0) return null
  return {
    cpu,
    ram: ((s.memory.total_kb - s.memory.available_kb) / s.memory.total_kb) * 100,
    temp: s.temperature_c ?? null,
    t: Date.now(),
  }
}
```

- [ ] **Step 4: Draw it**

In `SystemView.vue`, next to `cheminCpu` and `cheminRam`:

```ts
/**
 * Tracé de la température, en °C sur le **même axe 0-100** que les deux
 * pourcentages : les °C d'un Pi vivent dans cette plage (throttle à 80-85), la
 * mi-hauteur se lit donc « 50 °C » sans second repère, et `cheminSparkline`
 * borne déjà à 0-100 — une machine à plus de 100 °C s'aplatirait en haut du
 * cadre, ce qui est le moindre de ses problèmes. C'est la légende qui porte
 * l'unité, et c'est elle qui rend un axe mixte honnête.
 *
 * Une seule valeur manquante efface la courbe **entière** plutôt que de la
 * recoller par-dessus le trou : les trois tracés, le trait de survol et le
 * popin partagent un seul jeu d'abscisses (`abscissesGraphe`), et une série
 * plus courte dériverait des autres. Une machine sans sonde n'a donc jamais de
 * courbe, et un trou passager la fait disparaître le temps qu'il sorte du
 * tampon — visible, et honnête.
 */
const cheminTemp = computed(() => {
  const temps: number[] = []
  for (const e of historique.value) {
    if (e.temp === null) return ''
    temps.push(e.temp)
  }
  return cheminSparkline(temps, abscissesGraphe.value, HAUTEUR)
})
```

Add the third `<path>` **after** the RAM one and before the hover line, so the
existing `w.get('[data-system-history] path')` (which grabs the first path,
CPU) keeps working:

```html
            <!-- Troisième courbe distinguée par la couleur seule, sans
                 pointillé : `destructive` est la seule teinte garantie
                 distincte de `primary` et de `muted-foreground` dans les 42
                 presets du kit. Elle ne signale pas une alerte ici — c'est la
                 couleur d'une série, et la légende dit laquelle. -->
            <path
              data-system-history-temp
              :d="cheminTemp"
              class="text-destructive"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              vector-effect="non-scaling-stroke"
            />
```

Add the legend span, after the memory one:

```html
          <!-- Annoncée d'après `etat` et non d'après le dernier échantillon :
               l'existence d'une sonde est connue dès le premier sondage, donc
               la légende ne gagne pas une colonne en cours de route. La valeur,
               elle, vient bien de l'échantillon, comme les deux autres. -->
          <span v-if="etat?.temperature_c != null" class="text-destructive">
            {{ t('system_temperature') }}
            {{ dernier?.temp != null ? `${dernier.temp.toFixed(1)} °C` : RIEN }}
          </span>
```

And the popin line, after the memory one:

```html
            <div v-if="echantillonSurvol.temp !== null" class="text-destructive">
              {{ t('system_temperature') }} {{ echantillonSurvol.temp.toFixed(1) }} °C
            </div>
```

No new i18n key: `system_temperature` already exists (the CPU card uses it).
`°C` stays untranslated — an SI symbol, identical in both languages, as the
comment above `temperature` already records.

- [ ] **Step 5: Run the tests, then the whole suite**

Run, from `web/app`: `npx vitest run src/views/SystemView.test.ts`
Expected: PASS.

Run, from `web/app`: `npx vitest run`
Expected: PASS.

Run the project's type check (see `web/app/package.json`). Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add web/app/src/composables/useMetriques.ts web/app/src/views/SystemView.vue web/app/src/views/SystemView.test.ts
git commit -m "feat(web): temperature en troisieme courbe de l historique"
```

---

## Final verification (after Task 8)

- [ ] From `web/app`: `npx vitest run` — every file green, count >= 208 + the
  new tests.
- [ ] `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/metriques-continues && cargo test -p ritornello-core"` — 274 tests.
- [ ] `wsl -e bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/metriques-continues && cargo clippy -p ritornello-core --all-targets -- -D warnings"` — clean.
- [ ] The project's web type check — clean.
- [ ] Grep the touched files for comments that the new behaviour has made
      false. At plan time three were known and are handled by Tasks 5 and 7
      (the `sonder()` "stops when hidden" note, the `monte` "timer nobody could
      stop" note, the `CAPACITE` window note). A fourth to check: the comment
      on `logs` in `SystemView.vue` saying the list is read **once at mount** —
      Task 3 makes it "at mount and on each dialog open".
- [ ] There is a Playwright end-to-end suite in this repo. Check whether any
      journey asserts on the error card showing every line, or on the System
      tab's graph being empty on arrival; both change here. Report what you
      find rather than editing journeys blind.
