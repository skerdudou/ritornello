# IHM Vue 3 / shadcn-vue — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remplacer les trois surfaces HTML de ritornello (`/status`, `/plugins/radio/`, `/plugins/generic-input/`) par une SPA Vue 3 + shadcn-vue servie par le cœur en Rust, avec bascule clair/sombre et sélecteur des 42 thèmes tweakcn — les IHM d'admin devenant des modules ESM livrés par les plugins eux-mêmes, sans que le cœur connaisse un seul nom de plugin.

**Architecture:** Un espace de travail npm produit trois familles de bundles : le **kit** (`@ritornello/ui` : composants shadcn-vue, moteur de thèmes, `t()`, client `api`), le **shell** (routage, accueil, statut) et un **module par plugin**. Le shell déclare une **import map** qui résout `vue` et `@ritornello/ui` vers des fichiers à noms stables ; les quatre bundles marquent ces deux spécificateurs comme externes, donc une seule instance de Vue et un seul jeu de composants servent tout le monde, plugins compris. Les bundles sont embarqués dans les binaires Rust (`rust-embed` pour le répertoire hashé du shell, `include_str!` pour les deux fichiers à noms fixes de chaque plugin), ce qui laisse `cargo build` et `cross build` fonctionner sans Node. Le transport des modules de plugin réutilise le protocole d'admin existant, étendu de `GetAsset`/`GetCatalog` : le cœur relaie des octets et du JSON dont il ignore le sens, exactement comme il le fait déjà pour `GetData`.

**Tech Stack:** Vue 3 (Composition API, `<script setup>`), TypeScript, Vite 7, Tailwind CSS v4 (configuration CSS-first), shadcn-vue (sur `reka-ui`), `vue-router` 4, Vitest + `@vue/test-utils`, Playwright (chromium). Côté Rust : axum 0.7, `rust-embed`, tokio, serde/serde_json, `ritornello-proto`, `ritornello-plugin-sdk`, `ritornello-i18n`, tracing, tempfile (dev).

## Global Constraints

- **Périmètre iso-fonctionnel.** Aucune fonction métier ajoutée ni retirée. Seule exception explicitement voulue : `/` (aujourd'hui 404) devient l'accueil.
- **Le cœur et le shell ne contiennent aucun nom de plugin.** La navigation se construit depuis `/api/status`. Toute constante du type `"radio"` ou `"generic-input"` dans `ritornello-core` ou `web/app` est un échec de la task.
- Le workspace Rust doit compiler après **chaque** task ; `npm test --workspaces` doit passer après chaque task qui touche à `web/`.
- **Une seule commit par task** (dernière étape de la task).
- Messages de commit et commentaires de code **en français** ; les sujets de commit restent **sans accents** (convention de l'historique).
- Tests unitaires Rust en `#[cfg(test)] mod` **dans le fichier testé**. Tests JS en `*.test.ts` à côté du module testé.
- Aucune garde `std::sync::RwLock` ne traverse un `.await`.
- Vérification systématique sous WSL : `cargo test --workspace` **et** `cargo clippy --workspace --all-targets -- -D warnings`.
- Toute task qui change une dépendance Rust commite le `Cargo.lock` régénéré ; toute task qui change une dépendance npm commite le `package-lock.json` régénéré.
- **Node 20+** requis pour développer. `cargo build`/`cross build` ne doivent **jamais** invoquer npm.
- Les répertoires `dist/` sont **gitignorés**. `web/kit/src/themes/presets.json` est en revanche **commité** (c'est une source).
- `UI_CONTRACT = 1` pour tout ce plan. Aucune task ne l'incrémente.
- Défauts de thème : preset **`northern-lights`**, mode **`light`**. Il n'existe **pas** de mode `system`.
- Seule ressource externe tolérée à l'exécution : le **CDN de polices**, avec repli `system-ui`/`monospace` en fin de chaque pile.
- Cible ARM obligatoire en fin de parcours : `cross build --release --workspace --target armv7-unknown-linux-gnueabihf`.
- Hors périmètre (rappel de la spec) : authentification, éditeur de thème, PWA/service worker, embarquement des polices, plugin `console`, glisser-déposer des stations.

---

## File Structure

**Espace de travail npm**

- `package.json` (créer — Task 1) — racine, `workspaces: ["web/*", "crates/*/ui"]`, scripts `build`/`test` délégués.
- `web/kit/package.json`, `tsconfig.json` (créer — Task 1) — paquet `@ritornello/ui`.
- `web/kit/src/contract.ts` (créer — Task 1) — `UI_CONTRACT`.
- `web/kit/src/i18n.ts` (+ `.test.ts`) (créer — Task 1) — `createT()`, `t()`, interpolation.
- `web/kit/src/api.ts` (+ `.test.ts`) (créer — Task 1) — client `fetch` : `get`, `put`, `post`.
- `web/kit/src/lib/utils.ts` (créer — Task 3) — `cn()`.
- `web/kit/src/themes/presets.json` (créer — Task 2) — les 42 presets, **commité**.
- `web/kit/scripts/fetch-presets.mjs` (créer — Task 2) — conversion depuis l'amont.
- `web/kit/src/themes/engine.ts` (+ `.test.ts`) (créer — Task 2) — `applyTheme`, `fontFamilies`, `ensureFontLink`.
- `web/kit/src/theme.css` (créer — Task 3) — bloc `@theme inline`, partagé par le shell **et** chaque plugin.
- `web/kit/src/components/ui/**` (créer — Task 3) — composants shadcn-vue.
- `web/kit/src/index.ts` (créer — Task 3) — surface publique du paquet.
- `web/kit/vite.config.ts` (créer — Task 3) — build lib ESM, `vue` externe, sortie `dist/ui-kit.js`.

**Shell**

- `web/app/package.json`, `tsconfig.json`, `index.html` (créer — Task 4).
- `web/app/src/vue-entry.ts` (créer — Task 4) — réexport de `vue`, bâti à part en `assets/vue.js`.
- `web/app/vite.vue.config.ts` (créer — Task 4) — build du `vue.js` à nom stable.
- `web/app/vite.config.ts` (créer — Task 4) — build de l'app, externes, copie de `ui-kit.js`, injection de l'import map.
- `web/app/src/main.ts`, `src/App.vue`, `src/router.ts` (créer — Task 4).
- `web/app/src/app.css` (créer — Task 4) — passe Tailwind du shell (préflight + `@source` sur le kit).
- `web/app/src/boot.ts` (+ `.test.ts`) (créer — Task 4) — lecture de `window.__RITORNELLO_THEME__`, application avant montage.
- `web/app/src/components/ThemeToggle.vue`, `ThemePicker.vue` (+ `ThemePicker.test.ts`) (créer — Task 8).
- `web/app/src/composables/useTheme.ts` (+ `.test.ts`) (créer — Task 8), `useCatalog.ts` (créer — Task 9).
- `web/app/src/views/HomeView.vue` (+ `.test.ts`), `StatusView.vue` (créer en coquille Task 4, remplis Task 9).
- `web/app/src/views/PluginView.ts` (+ `.test.ts`) et `PluginRoute.vue` (créer — Task 4) — chargeur de module de plugin, vérification du contrat.
- `web/app/e2e/parcours.spec.ts`, `e2e/serve.mjs`, `playwright.config.ts` (créer — Task 13).

**Cœur (Rust)**

- `crates/ritornello-core/src/theme.rs` (créer — Task 5) — validation de forme, routes `/api/theme`.
- `crates/ritornello-core/src/state.rs` (modifier — Task 5) — `theme`, `mode`.
- `crates/ritornello-core/src/core.rs` (modifier — Task 5) — champs `theme`/`mode`, `set_theme`, `persist`.
- `crates/ritornello-core/build.rs` (créer — Task 6) — bouchon si `dist/` absent.
- `crates/ritornello-core/src/placeholder.rs` (créer — Task 6) — fabrication du bouchon, fonction pure testée, incluse par `build.rs`.
- `crates/ritornello-core/src/web.rs` (créer — Task 6) — `rust-embed`, route `/assets/*`, repli SPA, injection du thème.
- `crates/ritornello-core/src/status.rs` (modifier — Tasks 5, 6, 7, 9, 10) — état et routes ; **perd** `status_page`, `escape_html` et la route `/status` en Task 9.
- `crates/ritornello-core/src/main.rs` (modifier — Tasks 5, 6, 10) — canal `theme_tx`, bras `select!`, modules `web`/`placeholder`, cache d'actifs.
- `crates/ritornello-core/src/admin.rs` (modifier — Task 10) — `asset`/`catalog` dans `AdminBackend`, routes `ui.js`/`ui.css`/`api/i18n`, cache + `ETag`.
- `crates/ritornello-core/Cargo.toml`, `Cargo.lock` (modifier — Task 6) — `rust-embed`.

**Socle partagé (Rust)**

- `crates/ritornello-i18n/src/lib.rs` (modifier — Task 7) — `Catalog::entries()`.
- `crates/ritornello-proto/src/admin.rs` (modifier — Task 10) — `GetAsset`, `GetCatalog`, `Asset`, `Catalog`.
- `crates/ritornello-plugin-sdk/src/server.rs` (modifier — Task 10) — trait `AdminPlugin`.
- `crates/ritornello-plugin-sdk/src/client.rs` (modifier — Task 10) — `get_asset`, `get_catalog`.

**Plugins** — les deux crates suivent exactement la même structure.

- `crates/ritornello-plugin-{radio,generic-input}/build.rs` (créer — Task 10) — bouchon `ui/dist/`.
- `crates/ritornello-plugin-{radio,generic-input}/src/placeholder.rs` (créer — Task 10) — module ESM de bouchon, inclus par `build.rs`.
- `crates/ritornello-plugin-{radio,generic-input}/src/admin.rs` (modifier — Task 10) — `asset`/`catalog` remplacent `page` ; **perdent** `PAGE_KEYS`, la substitution `{{clé}}` et le garde-fou de caractères dangereux.
- `crates/ritornello-plugin-{radio,generic-input}/src/index.html` (**supprimer** — Task 10).
- `crates/ritornello-plugin-radio/ui/**` (créer — Task 11) — paquet npm du module IHM (`RadioAdmin.vue`).
- `crates/ritornello-plugin-generic-input/ui/**` (créer — Task 12) — paquet npm du module IHM (`InputAdmin.vue`, `preset-toml.ts`).

**Documentation et build**

- `deploy/build.sh` (créer — Task 14) — la chaîne en trois étapes.
- `README.md` (modifier — Task 14).
- `.gitignore` (modifier — Task 1).

---

### Task 1: Espace de travail npm et fondations du kit (`t()`, client `api`, contrat)

Pose l'outillage JS et les deux briques sans dépendance visuelle : la
résolution i18n et le client HTTP. Aucune ligne de Rust dans cette task, aucun
composant : le livrable est `npm test --workspaces` vert.

**Files:**
- Create: `package.json`, `web/kit/package.json`, `web/kit/tsconfig.json`, `web/kit/vitest.config.ts`
- Create: `web/kit/src/contract.ts`, `web/kit/src/i18n.ts`, `web/kit/src/i18n.test.ts`, `web/kit/src/api.ts`, `web/kit/src/api.test.ts`
- Modify: `.gitignore`

**Interfaces:**
- Produces:
  - `export const UI_CONTRACT: number` (= 1)
  - `export type Catalog = Record<string, string>`
  - `export function createT(catalog: Catalog): (key: string, params?: Record<string, string | number>) => string`
  - `export const api: { get<T>(url: string): Promise<T>; put(url: string, body: unknown): Promise<string | null>; post(url: string, body: unknown): Promise<string | null> }`
  - Convention de `put`/`post` : `null` = succès (204/2xx), sinon **le message d'erreur** (champ `error` d'un corps JSON de 422, ou `HTTP <code>` à défaut) — miroir exact du helper `put()` des pages actuelles.

- [ ] **Step 1: Créer la racine de l'espace de travail npm**

`package.json` :

```json
{
  "name": "ritornello-web",
  "private": true,
  "type": "module",
  "workspaces": ["web/kit", "web/app", "crates/*/ui"],
  "engines": { "node": ">=20" },
  "scripts": {
    "build": "npm run build --workspaces --if-present",
    "test": "npm run test --workspaces --if-present"
  }
}
```

Note : `web/app` et les `crates/*/ui` n'existent pas encore ; `--if-present`
et l'absence de répertoire rendent la commande tolérante jusqu'à leur
création.

- [ ] **Step 2: Créer le paquet du kit**

`web/kit/package.json` :

```json
{
  "name": "@ritornello/ui",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "main": "./src/index.ts",
  "exports": {
    ".": "./src/index.ts",
    "./theme.css": "./src/theme.css"
  },
  "scripts": {
    "test": "vitest run"
  },
  "devDependencies": {
    "typescript": "^5.6.0",
    "vitest": "^2.1.0"
  }
}
```

`web/kit/tsconfig.json` :

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "resolveJsonModule": true,
    "lib": ["ES2022", "DOM"],
    "types": ["vitest/globals"],
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["src", "scripts"]
}
```

`web/kit/vitest.config.ts` :

```ts
import { defineConfig } from 'vitest/config'

export default defineConfig({
  test: { environment: 'jsdom', globals: true },
})
```

Ajouter `jsdom` aux `devDependencies` du kit (`"jsdom": "^25.0.0"`).

- [ ] **Step 3: Écrire les tests de `t()` (doivent échouer : le module n'existe pas)**

`web/kit/src/i18n.test.ts` :

```ts
import { describe, expect, it } from 'vitest'
import { createT } from './i18n'

describe('createT', () => {
  it('résout une clé présente', () => {
    const t = createT({ saved: 'Enregistré' })
    expect(t('saved')).toBe('Enregistré')
  })

  it('retombe sur la clé elle-même quand elle est absente', () => {
    const t = createT({})
    expect(t('inconnue')).toBe('inconnue')
  })

  it('interpole les jetons nommés comme le fait le Rust', () => {
    const t = createT({ bad_request: 'Requête invalide : {detail}' })
    expect(t('bad_request', { detail: 'preset en double' })).toBe(
      'Requête invalide : preset en double',
    )
  })

  it('interpole un jeton numérique et laisse les jetons non fournis intacts', () => {
    const t = createT({ msg: '{n} sur {total}' })
    expect(t('msg', { n: 3 })).toBe('3 sur {total}')
  })

  it("n'interprète pas la valeur : une apostrophe droite passe telle quelle", () => {
    // C'est précisément ce que l'ancienne substitution `{{cle}}` cassait
    // (défaut Critical de dbfa771) : ici la valeur est une donnée, jamais du
    // source, donc aucun caractère n'est dangereux.
    const t = createT({ hint: "choisir d'abord un périphérique" })
    expect(t('hint')).toBe("choisir d'abord un périphérique")
  })
})
```

- [ ] **Step 4: Écrire les tests du client `api` (doivent échouer)**

`web/kit/src/api.test.ts` :

```ts
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from './api'

function mockFetch(response: Response) {
  const spy = vi.fn().mockResolvedValue(response)
  vi.stubGlobal('fetch', spy)
  return spy
}

describe('api', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('get renvoie le JSON décodé', async () => {
    mockFetch(new Response(JSON.stringify({ stations: [] }), { status: 200 }))
    await expect(api.get<{ stations: unknown[] }>('/x')).resolves.toEqual({ stations: [] })
  })

  it('get rejette sur un statut non ok', async () => {
    mockFetch(new Response('nope', { status: 502 }))
    await expect(api.get('/x')).rejects.toThrow('HTTP 502')
  })

  it('put renvoie null sur 204', async () => {
    const spy = mockFetch(new Response(null, { status: 204 }))
    await expect(api.put('/x', { a: 1 })).resolves.toBeNull()
    expect(spy).toHaveBeenCalledWith('/x', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ a: 1 }),
    })
  })

  it('put renvoie le message du champ error sur 422', async () => {
    mockFetch(new Response(JSON.stringify({ error: 'preset en double' }), { status: 422 }))
    await expect(api.put('/x', {})).resolves.toBe('preset en double')
  })

  it('put retombe sur HTTP <code> quand le corps n’est pas du JSON', async () => {
    mockFetch(new Response('plugin injoignable', { status: 502 }))
    await expect(api.put('/x', {})).resolves.toBe('HTTP 502')
  })

  it('post suit la même convention que put', async () => {
    const spy = mockFetch(new Response(null, { status: 204 }))
    await expect(api.post('/api/command', { cmd: 'VolumeUp' })).resolves.toBeNull()
    expect(spy.mock.calls[0]?.[1]).toMatchObject({ method: 'POST' })
  })
})
```

- [ ] **Step 5: Lancer les tests — échec attendu**

Run : `npm install && npm test --workspaces`
Expected : ÉCHEC — `Failed to resolve import "./i18n"` et `"./api"`.

- [ ] **Step 6: Écrire les trois modules**

`web/kit/src/contract.ts` :

```ts
/// Version du contrat que le cœur expose aux modules d'IHM des plugins
/// (`vue` + `@ritornello/ui`). Un module de plugin exporte son propre
/// `contract` ; le shell refuse de le monter en cas d'écart, avec un message
/// explicite. À incrémenter à toute modification incompatible du kit.
export const UI_CONTRACT = 1
```

`web/kit/src/i18n.ts` :

```ts
export type Catalog = Record<string, string>

/// Résolution d'une clé puis interpolation des jetons `{nom}`, en miroir de
/// ce que fait le Rust (`catalog.get(key)` puis `str::replace("{n}", …)`).
/// Clé absente : on renvoie la clé elle-même, exactement comme
/// `ritornello_i18n::Catalog::get`. Un jeton dont la valeur n'est pas fournie
/// reste tel quel, plutôt que de disparaître : un texte visiblement incomplet
/// est plus facile à diagnostiquer qu'un texte silencieusement tronqué.
export function createT(catalog: Catalog) {
  return (key: string, params?: Record<string, string | number>): string => {
    let out = catalog[key] ?? key
    if (params) {
      for (const [name, value] of Object.entries(params)) {
        out = out.replaceAll(`{${name}}`, String(value))
      }
    }
    return out
  }
}
```

`web/kit/src/api.ts` :

```ts
const JSON_HEADERS = { 'content-type': 'application/json' }

/// Renvoie `null` si l'opération est acceptée, sinon le message d'erreur —
/// le champ `error` du corps JSON d'un 422 quand il est là, `HTTP <code>`
/// sinon. Convention reprise telle quelle du helper `put()` des pages
/// actuelles, pour que les vues migrées n'aient pas à changer de logique.
async function send(method: 'PUT' | 'POST', url: string, body: unknown): Promise<string | null> {
  const r = await fetch(url, { method, headers: JSON_HEADERS, body: JSON.stringify(body) })
  if (r.ok) return null
  try {
    const j = (await r.json()) as { error?: string }
    if (j && typeof j.error === 'string') return j.error
  } catch {
    // corps non JSON : on retombe sur le code
  }
  return `HTTP ${r.status}`
}

export const api = {
  async get<T>(url: string): Promise<T> {
    const r = await fetch(url)
    if (!r.ok) throw new Error(`HTTP ${r.status}`)
    return (await r.json()) as T
  },
  put: (url: string, body: unknown) => send('PUT', url, body),
  post: (url: string, body: unknown) => send('POST', url, body),
}
```

- [ ] **Step 7: Ignorer les livrables de build**

Ajouter à `.gitignore` :

```
/node_modules
node_modules/
dist/
/web/app/test-results
/web/app/playwright-report
```

- [ ] **Step 8: Lancer les tests — succès attendu**

Run : `npm test --workspaces`
Expected : SUCCÈS — 11 tests passent.

- [ ] **Step 9: Commit**

```bash
git add package.json package-lock.json .gitignore web/kit
git commit -m "feat(web): espace de travail npm, contrat du kit, resolution i18n et client api"
```

---

### Task 2: Moteur de thèmes et import des 42 presets tweakcn

Le cœur battant du sujet : convertir les presets de l'amont en une source
locale, puis les appliquer en variables CSS. Aucun composant, aucune vue —
uniquement de la logique testée.

**Files:**
- Create: `web/kit/scripts/fetch-presets.mjs`, `web/kit/src/themes/presets.json`, `web/kit/src/themes/engine.ts`, `web/kit/src/themes/engine.test.ts`

**Interfaces:**
- Consumes: rien (task autonome).
- Produces:
  - `export type Mode = 'light' | 'dark'`
  - `export interface Preset { label: string; styles: { light: Record<string, string>; dark: Record<string, string> } }`
  - `export const presets: Record<string, Preset>` (42 entrées, dont `northern-lights`)
  - `export const DEFAULT_PRESET = 'northern-lights'`, `export const DEFAULT_MODE: Mode = 'light'`
  - `export function resolveVars(preset: Preset, mode: Mode): Record<string, string>`
  - `export function applyTheme(id: string, mode: Mode, root?: HTMLElement, doc?: Document): void`
  - `export function fontFamilies(vars: Record<string, string>): string[]`
  - `export function withFallback(key: string, value: string): string`

- [ ] **Step 1: Écrire le script d'import**

`web/kit/scripts/fetch-presets.mjs` — lancé **à la main** lors d'une mise à
jour de l'amont, jamais par le build :

```js
// Convertit `utils/theme-presets.ts` de tweakcn (Apache-2.0,
// https://github.com/jnsahaj/tweakcn) en `src/themes/presets.json`.
//
// L'amont est un module TypeScript dont le corps est un littéral d'objet pur :
// on le réduit en JSON en retirant l'import, la déclaration `export const` et
// les virgules traînantes, puis on le valide en le parsant. Volontairement
// naïf : si l'amont change de forme, le script échoue bruyamment plutôt que
// de produire un JSON partiel.
//
// Usage : node scripts/fetch-presets.mjs
import { writeFileSync } from 'node:fs'

const URL_AMONT =
  'https://raw.githubusercontent.com/jnsahaj/tweakcn/main/utils/theme-presets.ts'

const source = await fetch(URL_AMONT).then((r) => {
  if (!r.ok) throw new Error(`amont injoignable : HTTP ${r.status}`)
  return r.text()
})

const debut = source.indexOf('{', source.indexOf('defaultPresets'))
if (debut < 0) throw new Error("`defaultPresets` introuvable dans l'amont")
const corps = source
  .slice(debut)
  .replace(/;?\s*$/, '')
  .replace(/,(\s*[}\]])/g, '$1')          // virgules traînantes
  .replace(/([{,]\s*)([A-Za-z_$][\w$]*)\s*:/g, '$1"$2":')  // clés nues

const presets = JSON.parse(corps)
const noms = Object.keys(presets)
if (noms.length < 40) throw new Error(`trop peu de presets analysés : ${noms.length}`)
if (!presets['northern-lights']) throw new Error('`northern-lights` absent')
for (const [nom, p] of Object.entries(presets)) {
  if (!p.label || !p.styles?.light || !p.styles?.dark) {
    throw new Error(`preset ${nom} incomplet`)
  }
}

writeFileSync(
  new URL('../src/themes/presets.json', import.meta.url),
  JSON.stringify(presets, null, 2) + '\n',
)
console.log(`${noms.length} presets écrits`)
```

- [ ] **Step 2: Produire `presets.json` et l'inspecter**

Run : `cd web/kit && node scripts/fetch-presets.mjs`
Expected : `42 presets écrits`.

Vérifier à l'œil que `src/themes/presets.json` contient bien
`"northern-lights"` avec `"label": "Northern Lights"`, `styles.light.primary`
valant `"#34a85a"` et `styles.dark.background` valant `"#1a1d23"`.

Ajouter en tête du répertoire un fichier d'attribution
`web/kit/src/themes/ATTRIBUTION.md` :

```markdown
Les presets de `presets.json` proviennent de [tweakcn](https://tweakcn.com)
(dépôt `jnsahaj/tweakcn`), distribué sous licence **Apache-2.0**.

Régénérer avec `node scripts/fetch-presets.mjs`. Ne pas éditer `presets.json`
à la main : la modification serait perdue à la prochaine régénération.
```

- [ ] **Step 3: Écrire les tests du moteur (doivent échouer : le module n'existe pas)**

`web/kit/src/themes/engine.test.ts` :

```ts
import { beforeEach, describe, expect, it } from 'vitest'
import {
  applyTheme,
  DEFAULT_MODE,
  DEFAULT_PRESET,
  fontFamilies,
  presets,
  resolveVars,
  withFallback,
} from './engine'

describe('presets', () => {
  it('embarque les 42 presets de l’amont, dont le défaut', () => {
    expect(Object.keys(presets)).toHaveLength(42)
    expect(presets[DEFAULT_PRESET]?.label).toBe('Northern Lights')
    expect(DEFAULT_MODE).toBe('light')
  })

  it('chaque preset a un libellé et ses deux modes', () => {
    for (const [nom, p] of Object.entries(presets)) {
      expect(p.label, nom).toBeTruthy()
      expect(p.styles.light.background, nom).toBeTruthy()
      expect(p.styles.dark.background, nom).toBeTruthy()
    }
  })
})

describe('resolveVars', () => {
  it('superpose le bloc du mode sur le bloc clair', () => {
    const preset = {
      label: 'T',
      styles: { light: { background: '#fff', radius: '0.5rem' }, dark: { background: '#000' } },
    }
    const vars = resolveVars(preset, 'dark')
    expect(vars.background).toBe('#000')
    // `radius` n'est pas redéfini par le bloc sombre : il vient du bloc clair.
    expect(vars.radius).toBe('0.5rem')
  })

  it('en mode clair, le bloc sombre est ignoré', () => {
    const preset = {
      label: 'T',
      styles: { light: { background: '#fff' }, dark: { background: '#000' } },
    }
    expect(resolveVars(preset, 'light').background).toBe('#fff')
  })
})

describe('applyTheme', () => {
  let root: HTMLElement

  beforeEach(() => {
    document.head.innerHTML = ''
    root = document.createElement('div')
  })

  it('écrit chaque clé du preset en variable CSS', () => {
    applyTheme(DEFAULT_PRESET, 'light', root)
    expect(root.style.getPropertyValue('--background')).toBe('#f9f9fa')
    expect(root.style.getPropertyValue('--primary')).toBe('#34a85a')
    expect(root.style.getPropertyValue('--radius')).toBe('0.5rem')
  })

  it('applique le bloc sombre en mode sombre et pose la classe `dark`', () => {
    applyTheme(DEFAULT_PRESET, 'dark', root)
    expect(root.style.getPropertyValue('--background')).toBe('#1a1d23')
    expect(root.classList.contains('dark')).toBe(true)
    applyTheme(DEFAULT_PRESET, 'light', root)
    expect(root.classList.contains('dark')).toBe(false)
  })

  it('applique une clé inconnue sans broncher (itération générique)', () => {
    // Aucune liste de clés en dur : un preset amont qui gagne une variable
    // doit fonctionner sans toucher au code.
    const root2 = document.createElement('div')
    applyTheme('__test__', 'light', root2, document, {
      __test__: { label: 'T', styles: { light: { 'variable-inedite': '#123456' }, dark: {} } },
    })
    expect(root2.style.getPropertyValue('--variable-inedite')).toBe('#123456')
  })

  it('purge les variables du thème précédent', () => {
    applyTheme('__a__', 'light', root, document, {
      __a__: { label: 'A', styles: { light: { 'seulement-dans-a': '#111' }, dark: {} } },
    })
    expect(root.style.getPropertyValue('--seulement-dans-a')).toBe('#111')
    applyTheme('__b__', 'light', root, document, {
      __b__: { label: 'B', styles: { light: { background: '#222' }, dark: {} } },
    })
    expect(root.style.getPropertyValue('--seulement-dans-a')).toBe('')
  })

  it('ignore un identifiant de preset inconnu sans jeter', () => {
    applyTheme('preset-qui-nexiste-pas', 'light', root)
    expect(root.style.getPropertyValue('--background')).toBe('')
  })

  it('injecte un unique lien de polices et le remplace au changement', () => {
    applyTheme(DEFAULT_PRESET, 'light', root)
    const liens = () => [...document.head.querySelectorAll('link[data-ritornello-fonts]')]
    expect(liens()).toHaveLength(1)
    expect(liens()[0]?.getAttribute('href')).toContain('Plus+Jakarta+Sans')
    applyTheme('vercel', 'light', root)
    expect(liens()).toHaveLength(1)
  })
})

describe('polices', () => {
  it('extrait les familles citées, sans doublon', () => {
    const familles = fontFamilies({
      'font-sans': 'Plus Jakarta Sans, sans-serif',
      'font-mono': 'JetBrains Mono, monospace',
      'font-serif': 'Plus Jakarta Sans, serif',
      background: '#fff',
    })
    expect(familles).toEqual(['Plus Jakarta Sans', 'JetBrains Mono'])
  })

  it('ne retient pas les familles génériques seules', () => {
    expect(fontFamilies({ 'font-sans': 'system-ui, sans-serif' })).toEqual([])
  })

  it('ajoute un repli système à chaque pile de polices', () => {
    expect(withFallback('font-sans', 'Plus Jakarta Sans')).toBe(
      'Plus Jakarta Sans, system-ui, sans-serif',
    )
    expect(withFallback('font-mono', 'JetBrains Mono')).toBe('JetBrains Mono, ui-monospace, monospace')
    // Repli déjà présent : on ne le duplique pas.
    expect(withFallback('font-sans', 'Inter, sans-serif')).toBe('Inter, sans-serif')
    // Clé non typographique : valeur inchangée.
    expect(withFallback('background', '#fff')).toBe('#fff')
  })
})
```

- [ ] **Step 4: Lancer les tests — échec attendu**

Run : `npm test -w @ritornello/ui`
Expected : ÉCHEC — `Failed to resolve import "./engine"`.

- [ ] **Step 5: Écrire le moteur**

`web/kit/src/themes/engine.ts` :

```ts
import brut from './presets.json'

export type Mode = 'light' | 'dark'

export interface Preset {
  label: string
  styles: { light: Record<string, string>; dark: Record<string, string> }
}

export const presets = brut as unknown as Record<string, Preset>

export const DEFAULT_PRESET = 'northern-lights'
export const DEFAULT_MODE: Mode = 'light'

/// Familles génériques : citées par les presets mais jamais à télécharger.
const GENERIQUES = new Set([
  'sans-serif', 'serif', 'monospace', 'system-ui', 'ui-monospace', 'ui-serif',
  'ui-sans-serif', 'cursive', 'fantasy', 'inherit',
])

/// Repli ajouté en fin de pile par famille typographique, pour que l'IHM
/// reste lisible quand le CDN de polices est injoignable (appareil hors
/// ligne) : c'est la seule ressource externe de l'interface.
const REPLIS: Record<string, string> = {
  'font-sans': 'system-ui, sans-serif',
  'font-serif': 'ui-serif, serif',
  'font-mono': 'ui-monospace, monospace',
}

/// Le bloc `light` sert de base, le bloc du mode le surcharge : les blocs
/// `dark` de l'amont omettent le plus souvent les clés non chromatiques
/// (polices, rayon), qui doivent alors venir du bloc clair.
export function resolveVars(preset: Preset, mode: Mode): Record<string, string> {
  return { ...preset.styles.light, ...preset.styles[mode] }
}

export function withFallback(key: string, value: string): string {
  const repli = REPLIS[key]
  if (!repli) return value
  const deja = value
    .split(',')
    .some((part) => GENERIQUES.has(part.trim().toLowerCase()))
  return deja ? value : `${value}, ${repli}`
}

export function fontFamilies(vars: Record<string, string>): string[] {
  const out: string[] = []
  for (const key of Object.keys(REPLIS)) {
    const value = vars[key]
    if (!value) continue
    const premiere = value.split(',')[0]?.trim().replace(/^["']|["']$/g, '')
    if (!premiere || GENERIQUES.has(premiere.toLowerCase())) continue
    if (!out.includes(premiere)) out.push(premiere)
  }
  return out
}

/// Un seul lien de polices vit dans le document : il est remplacé à chaque
/// application de thème (marqué par `data-ritornello-fonts`). Aucune police
/// n'est embarquée dans les binaires — voir la spec.
function ensureFontLink(familles: string[], doc: Document): void {
  const existant = doc.head.querySelector('link[data-ritornello-fonts]')
  if (existant) existant.remove()
  if (familles.length === 0) return
  const familles_url = familles
    .map((f) => `family=${encodeURIComponent(f).replace(/%20/g, '+')}:wght@400;500;600;700`)
    .join('&')
  const link = doc.createElement('link')
  link.rel = 'stylesheet'
  link.setAttribute('data-ritornello-fonts', '')
  link.href = `https://fonts.googleapis.com/css2?${familles_url}&display=swap`
  doc.head.appendChild(link)
}

/// Clés posées par la dernière application, pour pouvoir les retirer : un
/// preset qui ne définit pas une variable ne doit pas hériter de celle du
/// preset précédent.
let posees: string[] = []

/// Écrit chaque entrée du preset résolu en variable CSS sur `root`, itération
/// **générique** : aucune liste de clés en dur, pour qu'un preset amont qui
/// gagne une variable fonctionne sans toucher au code.
///
/// `root`, `doc` et `catalogue` ne sont paramétrables que pour les tests.
export function applyTheme(
  id: string,
  mode: Mode,
  root: HTMLElement = document.documentElement,
  doc: Document = document,
  catalogue: Record<string, Preset> = presets,
): void {
  const preset = catalogue[id]
  if (!preset) {
    console.warn(`thème inconnu ignoré : ${id}`)
    return
  }
  for (const key of posees) root.style.removeProperty(`--${key}`)
  const vars = resolveVars(preset, mode)
  for (const [key, value] of Object.entries(vars)) {
    root.style.setProperty(`--${key}`, withFallback(key, value))
  }
  posees = Object.keys(vars)
  root.classList.toggle('dark', mode === 'dark')
  ensureFontLink(fontFamilies(vars), doc)
}
```

- [ ] **Step 6: Lancer les tests — succès attendu**

Run : `npm test -w @ritornello/ui`
Expected : SUCCÈS — les 11 tests de la Task 1 plus les 14 du moteur.

- [ ] **Step 7: Commit**

```bash
git add web/kit/scripts web/kit/src/themes
git commit -m "feat(web): moteur de themes et import des 42 presets tweakcn (Apache-2.0)"
```

---

### Task 3: Composants shadcn-vue du kit, feuille de thème partagée et build ESM externalisé

Le kit devient utilisable : composants shadcn-vue, `cn()`, la feuille
`theme.css` que **le shell et chaque plugin** importeront, et le build en
bibliothèque ESM avec `vue` externe. C'est ce build qui produit le
`ui-kit.js` à nom stable désigné par l'import map.

**Files:**
- Create: `web/kit/src/lib/utils.ts`, `web/kit/src/theme.css`, `web/kit/src/index.ts`, `web/kit/vite.config.ts`, `web/kit/components.json`
- Create: `web/kit/src/components/ui/**` (via la CLI shadcn-vue)
- Create: `web/kit/src/index.test.ts`
- Modify: `web/kit/package.json`, `web/kit/tsconfig.json`, `package-lock.json`

**Interfaces:**
- Consumes: `UI_CONTRACT`, `createT`, `api` (Task 1) ; `applyTheme`, `presets`, `Mode`, `DEFAULT_PRESET`, `DEFAULT_MODE`, `Preset` (Task 2).
- Produces: la surface publique de `@ritornello/ui`, réexportant tout ce qui
  précède plus `cn` et les composants `Button`, `Input`, `Label`, `Select*`,
  `Table*`, `Card*`, `Dialog*`, `Switch`, `Badge`, `ScrollArea`, `Toaster` /
  `toast`. Livrable de build : `web/kit/dist/ui-kit.js` (ESM, `vue` externe).

- [ ] **Step 1: Installer les dépendances du kit**

```bash
npm i -w @ritornello/ui reka-ui class-variance-authority clsx tailwind-merge lucide-vue-next vue-sonner
npm i -w @ritornello/ui -D vue tailwindcss @tailwindcss/vite vite @vitejs/plugin-vue vue-tsc @vue/test-utils
```

`vue` est en `devDependencies` **et** en `peerDependencies` : le kit compile
contre lui mais ne l'embarque jamais (il est fourni par l'import map).
Ajouter à `web/kit/package.json` :

```json
  "peerDependencies": { "vue": "^3.5.0" },
  "scripts": { "build": "vite build", "test": "vitest run" }
```

- [ ] **Step 2: Configurer les chemins et la CLI shadcn-vue**

Ajouter à `web/kit/tsconfig.json`, dans `compilerOptions` :

```json
    "baseUrl": ".",
    "paths": { "@/*": ["./src/*"] },
    "jsx": "preserve"
```

et remplacer `"include": ["src", "scripts"]` par `"include": ["src", "scripts", "*.ts"]`.

`web/kit/components.json` :

```json
{
  "$schema": "https://shadcn-vue.com/schema.json",
  "style": "new-york",
  "typescript": true,
  "tailwind": { "config": "", "css": "src/theme.css", "baseColor": "neutral", "cssVariables": true },
  "aliases": { "components": "@/components", "composables": "@/composables", "utils": "@/lib/utils", "ui": "@/components/ui", "lib": "@/lib" }
}
```

`web/kit/src/lib/utils.ts` :

```ts
import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
```

Les tests du kit vont désormais monter des composants `.vue` : remplacer
`web/kit/vitest.config.ts` (écrit en Task 1 sans plugin Vue) par

```ts
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [vue()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  test: { environment: 'jsdom', globals: true },
})
```

Sans le plugin, `mount(Button)` échouerait sur l'import d'un `.vue`.

- [ ] **Step 3: Écrire la feuille de thème partagée**

`web/kit/src/theme.css` — **importée par le shell et par chaque module de
plugin**. Elle ne contient que le pont entre les variables posées par le
moteur de thèmes (`--background`, `--primary`…) et les espaces de noms
Tailwind v4 (`--color-*`, `--radius-*`). Volontairement **sans** `@import
"tailwindcss"` : c'est à l'appelant de décider s'il veut le préflight (le
shell oui, les plugins non, pour ne pas réinitialiser deux fois) :

```css
/* Pont entre les variables du moteur de themes et les espaces de noms
   Tailwind v4. Aucune valeur en dur ici : les couleurs viennent du preset
   applique a l'execution sur `documentElement`.
   Les variables typographiques (`--font-sans`, `--font-serif`, `--font-mono`)
   ne sont pas listees : elles appartiennent deja a l'espace de noms
   `--font-*` de Tailwind v4, donc le moteur les surcharge directement. */
@theme inline {
  --color-background: var(--background);
  --color-foreground: var(--foreground);
  --color-card: var(--card);
  --color-card-foreground: var(--card-foreground);
  --color-popover: var(--popover);
  --color-popover-foreground: var(--popover-foreground);
  --color-primary: var(--primary);
  --color-primary-foreground: var(--primary-foreground);
  --color-secondary: var(--secondary);
  --color-secondary-foreground: var(--secondary-foreground);
  --color-muted: var(--muted);
  --color-muted-foreground: var(--muted-foreground);
  --color-accent: var(--accent);
  --color-accent-foreground: var(--accent-foreground);
  --color-destructive: var(--destructive);
  --color-destructive-foreground: var(--destructive-foreground);
  --color-border: var(--border);
  --color-input: var(--input);
  --color-ring: var(--ring);
  --color-sidebar: var(--sidebar);
  --color-sidebar-foreground: var(--sidebar-foreground);
  --color-sidebar-primary: var(--sidebar-primary);
  --color-sidebar-primary-foreground: var(--sidebar-primary-foreground);
  --color-sidebar-accent: var(--sidebar-accent);
  --color-sidebar-accent-foreground: var(--sidebar-accent-foreground);
  --color-sidebar-border: var(--sidebar-border);
  --color-sidebar-ring: var(--sidebar-ring);
  --radius-sm: calc(var(--radius) - 4px);
  --radius-md: calc(var(--radius) - 2px);
  --radius-lg: var(--radius);
  --radius-xl: calc(var(--radius) + 4px);
}
```

- [ ] **Step 4: Générer les composants**

```bash
cd web/kit
npx --yes shadcn-vue@latest add -y -o button input label select table card dialog switch badge scroll-area sonner
```

`-y` et `-o` (overwrite) sont **indispensables** : la commande doit s'exécuter
sans aucune question, sinon elle reste bloquée sur une invite. Si la CLI refuse
malgré tout de partir (absence de `tailwind.config`, qui n'existe pas en
Tailwind v4), déposer les composants à la main depuis
`https://www.shadcn-vue.com/docs/components/<nom>` sous
`src/components/ui/<nom>/`, en conservant leurs imports `@/lib/utils` et
`reka-ui` — et le signaler dans le rapport de task.

La CLI dépose les composants sous `src/components/ui/<nom>/`. Vérifier qu'ils
importent bien `@/lib/utils` et `reka-ui`, et **ne pas les retoucher** ensuite :
ce sont des sources générées, mises à jour par la CLI.

- [ ] **Step 5: Écrire la surface publique**

`web/kit/src/index.ts` :

```ts
export { UI_CONTRACT } from './contract'
export { createT, type Catalog } from './i18n'
export { api } from './api'
export { cn } from './lib/utils'
export {
  applyTheme,
  DEFAULT_MODE,
  DEFAULT_PRESET,
  fontFamilies,
  presets,
  resolveVars,
  withFallback,
  type Mode,
  type Preset,
} from './themes/engine'

export { Button } from './components/ui/button'
export { Input } from './components/ui/input'
export { Label } from './components/ui/label'
export { Badge } from './components/ui/badge'
export { Switch } from './components/ui/switch'
export { ScrollArea } from './components/ui/scroll-area'
export { Card, CardContent, CardDescription, CardHeader, CardTitle } from './components/ui/card'
export {
  Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle, DialogTrigger,
} from './components/ui/dialog'
export {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from './components/ui/select'
export {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from './components/ui/table'
export { Toaster } from './components/ui/sonner'
export { toast } from 'vue-sonner'
```

- [ ] **Step 6: Écrire le test de fumée**

`web/kit/src/index.test.ts` :

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { Button, cn, UI_CONTRACT } from './index'

describe('surface publique du kit', () => {
  it('expose la version de contrat', () => {
    expect(UI_CONTRACT).toBe(1)
  })

  it('cn fusionne les classes en respectant la dernière', () => {
    expect(cn('p-2', 'p-4')).toBe('p-4')
  })

  it('monte un Button avec son contenu', () => {
    const w = mount(Button, { slots: { default: 'Enregistrer' } })
    expect(w.text()).toBe('Enregistrer')
    expect(w.element.tagName).toBe('BUTTON')
  })
})
```

- [ ] **Step 7: Écrire la configuration de build du kit**

`web/kit/vite.config.ts` :

```ts
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'

// Build en bibliotheque ESM. `vue` est **externe** : il est fourni par
// l'import map du shell, pour qu'une seule instance serve le shell et tous
// les modules de plugin. Le nom de sortie est **stable** (pas de hash) :
// c'est l'URL que l'import map designe et contre laquelle les plugins sont
// compiles.
export default defineConfig({
  plugins: [vue()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: {
    lib: { entry: 'src/index.ts', formats: ['es'], fileName: () => 'ui-kit.js' },
    rollupOptions: { external: ['vue'] },
    cssCodeSplit: false,
    emptyOutDir: true,
  },
})
```

- [ ] **Step 8: Lancer les tests et le build — succès attendu**

Run : `npm test -w @ritornello/ui && npm run build -w @ritornello/ui`
Expected : SUCCÈS.

Vérifier ensuite que `vue` n'est **pas** embarqué dans le bundle du kit :

```bash
node --input-type=module -e "
import { readFileSync } from 'node:fs'
const s = readFileSync('web/kit/dist/ui-kit.js', 'utf8')
if (!/from ?[\"']vue[\"']/.test(s)) throw new Error('vue devrait rester un import externe')
console.log('ui-kit.js', (s.length / 1024).toFixed(0), 'Ko, vue externe OK')
"
```

- [ ] **Step 9: Commit**

```bash
git add web/kit package-lock.json
git commit -m "feat(web): composants shadcn-vue, feuille de theme partagee et build ESM du kit"
```

---

### Task 4: Shell de l'app — import map, `vue.js` à nom stable, routage et amorçage du thème

Produit le bundle du shell et, avec lui, le mécanisme qui rend tout le reste
possible : l'import map et le partage de `vue`. Les vues Accueil et Statut
sont encore des coquilles (remplies en Task 9) ; le livrable est un `dist/`
complet et cohérent, plus le chargeur de module de plugin avec sa vérification
de contrat.

**Files:**
- Create: `web/app/package.json`, `tsconfig.json`, `index.html`, `vite.config.ts`, `vite.vue.config.ts`, `vitest.config.ts`
- Create: `web/app/src/vue-entry.ts`, `src/main.ts`, `src/App.vue`, `src/router.ts`, `src/app.css`, `src/types.ts`, `src/boot.ts`, `src/boot.test.ts`
- Create: `web/app/src/views/HomeView.vue`, `src/views/StatusView.vue`, `src/views/PluginRoute.vue`, `src/views/PluginView.ts`, `src/views/PluginView.test.ts`
- Modify: `package-lock.json`

**Interfaces:**
- Consumes: toute la surface de `@ritornello/ui` (Task 3).
- Produces:
  - `web/app/dist/index.html` (porteur de l'import map), `dist/assets/app-<hash>.js`, `dist/assets/app-<hash>.css`, `dist/assets/vue.js`, `dist/assets/ui-kit.js`
  - `export function readBootTheme(win?: Window): ThemePayload`
  - `export interface PluginModule { contract: number; default: Component }`
  - `src/types.ts` : `PluginStatus`, `StatusPayload`, `AudioPayload`, `LocalePayload`, `ThemePayload` — charges utiles partagées avec les routes du cœur.

- [ ] **Step 1: Créer le paquet du shell**

```bash
npm i -w app vue vue-router @ritornello/ui
npm i -w app -D vite @vitejs/plugin-vue tailwindcss @tailwindcss/vite typescript vue-tsc vitest jsdom @vue/test-utils
```

`web/app/package.json` :

```json
{
  "name": "app",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "vite build && vite build --config vite.vue.config.ts",
    "test": "vitest run"
  }
}
```

L'ordre des deux `vite build` est **significatif** : le premier vide `dist/`
et produit l'app, le second y ajoute `vue.js` sans vider (`emptyOutDir: false`
plus bas). Inverser les deux effacerait `vue.js`.

`web/app/tsconfig.json` : copie de celui du kit, avec
`"paths": { "@/*": ["./src/*"] }`, `"jsx": "preserve"` et
`"types": ["vitest/globals"]`.

`web/app/vitest.config.ts` — le plugin Vue est **nécessaire** (les tests
montent des `.vue` et un `.tsx`) :

```ts
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [vue()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  test: { environment: 'jsdom', globals: true },
})
```

`PluginView` est un `.ts`, pas un `.tsx` : son rendu est écrit avec `h()`, sans
syntaxe JSX — aucune chaîne de compilation JSX n'est donc nécessaire.

- [ ] **Step 2: Écrire le point d'entrée `vue.js` et sa configuration de build**

`web/app/src/vue-entry.ts` :

```ts
// Reexport de Vue, bati a part sous un nom **stable** (`assets/vue.js`) pour
// etre la cible de l'import map. Tous les bundles (shell, kit, modules de
// plugin) marquent `vue` comme externe et le resolvent ici : une seule
// instance de Vue vit dans la page, donc une seule reactivite et un seul
// arbre de `provide`/`inject`.
export * from 'vue'
```

`web/app/vite.vue.config.ts` :

```ts
import { defineConfig } from 'vite'

export default defineConfig({
  build: {
    lib: { entry: 'src/vue-entry.ts', formats: ['es'], fileName: () => 'vue.js' },
    outDir: 'dist/assets',
    // Le build de l'app est passe avant : ne pas vider, sinon on efface
    // `index.html` et les chunks hashes.
    emptyOutDir: false,
    copyPublicDir: false,
  },
})
```

- [ ] **Step 3: Écrire le shell HTML et la configuration de build de l'app**

`web/app/index.html` — l'import map est injectée au build :

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>ritornello</title>
    <!--IMPORTMAP-->
  </head>
  <body>
    <div id="app"></div>
    <script type="module" src="/src/main.ts"></script>
  </body>
</html>
```

`web/app/vite.config.ts` :

```ts
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { copyFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig, type Plugin } from 'vite'

const IMPORT_MAP = `<script type="importmap">
      {"imports":{"vue":"/assets/vue.js","@ritornello/ui":"/assets/ui-kit.js"}}
    </script>`

/// Injecte l'import map dans le shell et recopie le bundle du kit a cote des
/// actifs de l'app, pour que le coeur n'ait qu'un seul repertoire a embarquer.
function shellPlugin(): Plugin {
  return {
    name: 'ritornello-shell',
    transformIndexHtml(html) {
      return html.replace('<!--IMPORTMAP-->', IMPORT_MAP)
    },
    closeBundle() {
      copyFileSync(
        fileURLToPath(new URL('../kit/dist/ui-kit.js', import.meta.url)),
        fileURLToPath(new URL('./dist/assets/ui-kit.js', import.meta.url)),
      )
    },
  }
}

export default defineConfig({
  plugins: [vue(), tailwindcss(), shellPlugin()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: {
    // `vue` et le kit sont fournis par l'import map : les externaliser ici
    // aussi est ce qui garantit l'unicite de l'instance de Vue.
    rollupOptions: {
      external: ['vue', '@ritornello/ui'],
      output: {
        entryFileNames: 'assets/app-[hash].js',
        assetFileNames: 'assets/app-[hash][extname]',
      },
    },
    emptyOutDir: true,
  },
})
```

Le build du kit doit précéder celui de l'app (le `closeBundle` lit
`web/kit/dist/ui-kit.js`) : c'est déjà l'ordre imposé par
`npm run build --workspaces`, qui respecte le graphe de dépendances.

- [ ] **Step 4: Écrire la passe Tailwind du shell**

`web/app/src/app.css` :

```css
@import "tailwindcss";
@import "@ritornello/ui/theme.css";

/* Les composants du kit vivent hors de ce paquet : sans cette directive,
   Tailwind ne verrait pas leurs classes et le CSS du shell serait incomplet
   (boutons et tables sans style, y compris quand ce sont des vues de plugin
   qui les emploient). */
@source "../../kit/src";

body {
  @apply bg-background text-foreground;
  font-family: var(--font-sans);
}
```

- [ ] **Step 5: Écrire les tests de l'amorçage et du chargeur de plugin (doivent échouer)**

`web/app/src/boot.test.ts` :

```ts
import { DEFAULT_MODE, DEFAULT_PRESET } from '@ritornello/ui'
import { describe, expect, it } from 'vitest'
import { readBootTheme } from './boot'

describe('readBootTheme', () => {
  it('lit le choix injecté par le cœur dans le shell', () => {
    const win = { __RITORNELLO_THEME__: { theme: 'cyberpunk', mode: 'dark' } } as never
    expect(readBootTheme(win)).toEqual({ theme: 'cyberpunk', mode: 'dark' })
  })

  it('retombe sur les défauts quand rien n’est injecté', () => {
    expect(readBootTheme({} as never)).toEqual({ theme: DEFAULT_PRESET, mode: DEFAULT_MODE })
  })

  it('rejette un mode inconnu plutôt que de le propager', () => {
    const win = { __RITORNELLO_THEME__: { theme: 'vercel', mode: 'system' } } as never
    expect(readBootTheme(win)).toEqual({ theme: 'vercel', mode: DEFAULT_MODE })
  })
})
```

`web/app/src/views/PluginView.test.ts` :

```ts
import { flushPromises, mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { defineComponent, h } from 'vue'
import PluginView from './PluginView'

const CATALOGUE = {
  indisponible: 'IHM indisponible',
  contrat: 'Plugin à reconstruire',
  loading: 'Chargement…',
}

function monter(loader: () => Promise<unknown>) {
  return mount(PluginView, { props: { name: 'demo', loadModule: loader, catalog: CATALOGUE } })
}

describe('PluginView', () => {
  it('monte le composant du module quand le contrat correspond', async () => {
    const vue = defineComponent({ render: () => h('p', 'IHM du plugin') })
    const w = monter(async () => ({ contract: 1, default: vue }))
    await flushPromises()
    expect(w.text()).toContain('IHM du plugin')
  })

  it('refuse un contrat incompatible avec un message explicite', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const vue = defineComponent({ render: () => h('p', 'ne doit pas apparaître') })
    const w = monter(async () => ({ contract: 99, default: vue }))
    await flushPromises()
    expect(w.text()).toContain('Plugin à reconstruire')
    expect(w.text()).not.toContain('ne doit pas apparaître')
  })

  it('affiche l’indisponibilité quand le module ne charge pas', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = monter(async () => {
      throw new Error('404')
    })
    await flushPromises()
    expect(w.text()).toContain('IHM indisponible')
  })

  it('affiche l’indisponibilité quand le module n’exporte pas de composant', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const w = monter(async () => ({ contract: 1 }))
    await flushPromises()
    expect(w.text()).toContain('IHM indisponible')
  })

  it('injecte la feuille de style du plugin une seule fois', async () => {
    document.head.innerHTML = ''
    const vue = defineComponent({ render: () => h('p', 'ok') })
    monter(async () => ({ contract: 1, default: vue }))
    monter(async () => ({ contract: 1, default: vue }))
    await flushPromises()
    expect(document.head.querySelectorAll('link[href="/plugins/demo/ui.css"]')).toHaveLength(1)
  })
})
```

- [ ] **Step 6: Lancer les tests — échec attendu**

Run : `npm test -w app`
Expected : ÉCHEC — `Failed to resolve import "./boot"` et `"./PluginView"`.

- [ ] **Step 7: Écrire les types, l'amorçage et le chargeur de plugin**

`web/app/src/types.ts` :

```ts
import type { Mode } from '@ritornello/ui'

export interface PluginStatus { name: string; kind: string; connected: boolean; admin: boolean }
export interface StatusPayload { plugins: PluginStatus[]; active_source: string }
export interface AudioPayload { devices: string[]; current: string | null }
export interface LocalePayload { locales: string[]; current: string | null }
export interface ThemePayload { theme: string; mode: Mode }
```

`web/app/src/boot.ts` :

```ts
import { DEFAULT_MODE, DEFAULT_PRESET, type Mode } from '@ritornello/ui'
import type { ThemePayload } from './types'

declare global {
  interface Window {
    __RITORNELLO_THEME__?: { theme?: string; mode?: string }
  }
}

/// Le coeur injecte le choix persiste directement dans le shell qu'il sert :
/// le theme est donc applique des le premier rendu, sans attendre un
/// aller-retour `GET /api/theme` — pas de clignotement. Le coeur ne
/// transporte que deux chaines ; il ne connait aucune couleur.
export function readBootTheme(win: Window = window): ThemePayload {
  const brut = win.__RITORNELLO_THEME__
  const mode: Mode = brut?.mode === 'dark' || brut?.mode === 'light' ? brut.mode : DEFAULT_MODE
  return { theme: brut?.theme || DEFAULT_PRESET, mode }
}
```

`web/app/src/views/PluginView.ts` — une fonction de rendu (`h()`) plutôt qu'un
SFC, parce que le rendu est purement conditionnel et que le composant monté est
une valeur dynamique :

```ts
import { createT, UI_CONTRACT, type Catalog } from '@ritornello/ui'
import { defineComponent, h, ref, watchEffect, type Component, type PropType } from 'vue'

export interface PluginModule {
  contract: number
  default: Component
}

/// Le CSS d'un plugin est sa propre passe Tailwind : on l'injecte une fois et
/// on le laisse en place (revenir sur la page ne doit pas rejouer un
/// telechargement).
function ensureStylesheet(name: string): void {
  const href = `/plugins/${name}/ui.css`
  if (document.head.querySelector(`link[href="${href}"]`)) return
  const link = document.createElement('link')
  link.rel = 'stylesheet'
  link.href = href
  document.head.appendChild(link)
}

/// Charge le module d'IHM d'un plugin et le monte. Le nom du plugin vient de
/// `/api/status` : ni ce fichier ni le coeur ne connaissent la liste des
/// plugins. `loadModule` n'est parametrable que pour les tests ; en
/// production c'est un `import()` dynamique de `/plugins/<nom>/ui.js`.
export default defineComponent({
  name: 'PluginView',
  props: {
    name: { type: String, required: true },
    catalog: { type: Object as PropType<Catalog>, default: () => ({}) },
    loadModule: {
      type: Function as PropType<(name: string) => Promise<unknown>>,
      default: (name: string) => import(/* @vite-ignore */ `/plugins/${name}/ui.js`),
    },
  },
  setup(props) {
    const composant = ref<Component | null>(null)
    const erreur = ref<'indisponible' | 'contrat' | null>(null)

    watchEffect(async () => {
      composant.value = null
      erreur.value = null
      ensureStylesheet(props.name)
      try {
        const mod = (await props.loadModule(props.name)) as Partial<PluginModule>
        if (mod?.contract !== UI_CONTRACT) {
          console.warn(`plugin ${props.name}: contrat ${mod?.contract} attendu ${UI_CONTRACT}`)
          erreur.value = 'contrat'
          return
        }
        if (!mod.default) {
          console.warn(`plugin ${props.name}: aucun composant par defaut exporte`)
          erreur.value = 'indisponible'
          return
        }
        composant.value = mod.default
      } catch (e) {
        console.warn(`plugin ${props.name}: chargement impossible`, e)
        erreur.value = 'indisponible'
      }
    })

    return () => {
      const t = createT(props.catalog)
      if (erreur.value) return h('p', { class: 'text-muted-foreground' }, t(erreur.value))
      if (!composant.value) return h('p', { class: 'text-muted-foreground' }, t('loading'))
      return h(composant.value)
    }
  },
})
```

- [ ] **Step 8: Écrire le routage, `App.vue` et le point d'entrée**

`web/app/src/router.ts` :

```ts
import { createRouter, createWebHistory } from 'vue-router'

// Les URL historiques sont conservees : `/status` et `/plugins/<nom>/`
// repondaient deja, le coeur les sert desormais par repli sur le shell.
export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', name: 'home', component: () => import('./views/HomeView.vue') },
    { path: '/status', name: 'status', component: () => import('./views/StatusView.vue') },
    {
      path: '/plugins/:name/',
      name: 'plugin',
      component: () => import('./views/PluginRoute.vue'),
    },
  ],
})
```

`web/app/src/views/PluginRoute.vue` — enveloppe qui récupère le catalogue du
plugin avant de déléguer à `PluginView` :

```vue
<script setup lang="ts">
import { api, type Catalog } from '@ritornello/ui'
import { ref, watch } from 'vue'
import { useRoute } from 'vue-router'
import PluginView from './PluginView'

const route = useRoute()
const name = ref('')
const catalog = ref<Catalog>({})

watch(
  () => route.params.name,
  async (valeur) => {
    name.value = String(valeur ?? '')
    if (!name.value) return
    // Un catalogue injoignable ne doit pas empecher l'IHM de s'afficher :
    // `t()` retombe alors sur les cles, ce qui reste lisible.
    catalog.value = await api.get<Catalog>(`/plugins/${name.value}/api/i18n`).catch(() => ({}))
  },
  { immediate: true },
)
</script>

<template>
  <PluginView v-if="name" :key="name" :name="name" :catalog="catalog" />
</template>
```

`web/app/src/App.vue` — l'en-tête est complété en Task 8 (thème) ; la
navigation est construite depuis `/api/status`, donc sans aucun nom de plugin
en dur :

```vue
<script setup lang="ts">
import { api, Toaster } from '@ritornello/ui'
import { onMounted, ref } from 'vue'
import { RouterLink, RouterView } from 'vue-router'
import type { StatusPayload } from './types'

const admins = ref<string[]>([])

onMounted(async () => {
  const s = await api.get<StatusPayload>('/api/status').catch(() => null)
  admins.value = (s?.plugins ?? []).filter((p) => p.admin).map((p) => p.name)
})
</script>

<template>
  <div class="min-h-screen">
    <header class="border-b border-border">
      <nav class="mx-auto flex max-w-3xl items-center gap-4 px-4 py-3">
        <RouterLink to="/" class="font-semibold">ritornello</RouterLink>
        <RouterLink to="/status" class="text-sm text-muted-foreground">status</RouterLink>
        <RouterLink
          v-for="name in admins"
          :key="name"
          :to="`/plugins/${name}/`"
          class="text-sm text-muted-foreground"
        >
          {{ name }}
        </RouterLink>
        <span class="ml-auto" />
      </nav>
    </header>
    <main class="mx-auto max-w-3xl px-4 py-6">
      <RouterView />
    </main>
    <Toaster />
  </div>
</template>
```

`web/app/src/main.ts` :

```ts
import { applyTheme } from '@ritornello/ui'
import { createApp } from 'vue'
import App from './App.vue'
import './app.css'
import { readBootTheme } from './boot'
import { router } from './router'

// Le theme est applique **avant** le montage : le premier rendu est deja
// dans les bonnes couleurs.
const { theme, mode } = readBootTheme()
applyTheme(theme, mode)

createApp(App).use(router).mount('#app')
```

Coquilles provisoires, remplies en Task 9 — `web/app/src/views/HomeView.vue`
et `web/app/src/views/StatusView.vue`, identiques :

```vue
<template>
  <p class="text-muted-foreground">à venir</p>
</template>
```

- [ ] **Step 9: Lancer les tests et le build — succès attendu**

Run : `npm test --workspaces && npm run build --workspaces`
Expected : SUCCÈS.

Vérifier la cohérence du `dist/` :

```bash
ls web/app/dist web/app/dist/assets
node --input-type=module -e "
import { readFileSync } from 'node:fs'
const s = readFileSync('web/app/dist/index.html', 'utf8')
if (!s.includes('/assets/vue.js')) throw new Error('import map absente')
if (s.includes('IMPORTMAP')) throw new Error('marqueur non substitue')
console.log('import map injectee')
"
```

Attendu : `index.html`, `assets/vue.js`, `assets/ui-kit.js`,
`assets/app-<hash>.js`, `assets/app-<hash>.css`, et le message
`import map injectee`.

- [ ] **Step 10: Commit**

```bash
git add web/app package-lock.json
git commit -m "feat(web): shell de la SPA, import map, partage de vue et chargeur de module de plugin"
```

---

### Task 5: Thème côté serveur — `GET`/`PUT /api/theme` et persistance dans `state.json`

Task **purement Rust**, sans rapport avec les bundles : le thème est un
réglage de l'appareil, il se persiste et se sert comme la locale. Elle vient
avant l'embarquement (Task 6) parce que le shell servi doit injecter un choix
qui existe déjà côté serveur.

**Files:**
- Create: `crates/ritornello-core/src/theme.rs`
- Modify: `crates/ritornello-core/src/state.rs`, `src/core.rs`, `src/status.rs`, `src/main.rs`

**Interfaces:**
- Consumes: `PersistedState`, `state::save` (existants) ; le motif exact de
  `PUT /api/locale` → canal → bras `select!` → `Core::set_locale`.
- Produces:
  - `pub const DEFAULT_THEME: &str = "northern-lights"`, `pub const DEFAULT_MODE: &str = "light"`
  - `pub struct ThemeState { pub theme: String, pub mode: String }` (`Serialize`, `Deserialize`, `Clone`, `PartialEq`)
  - `pub fn validate(theme: &str, mode: &str) -> Result<(), String>`
  - `AppState` gagne `theme_current: Arc<RwLock<ThemeState>>` et `theme_tx: mpsc::Sender<ThemeState>`
  - `Core::set_theme(&mut self, t: ThemeState)`
  - Routes `GET /api/theme` → `{theme, mode}` et `PUT /api/theme` → 204 / 422

- [ ] **Step 1: Écrire les tests de validation et de persistance (doivent échouer)**

`crates/ritornello-core/src/theme.rs`, dans un `#[cfg(test)] mod tests` — le
fichier ne contient encore **que** ce module et les `use` :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn les_defauts_sont_northern_lights_en_clair() {
        assert_eq!(DEFAULT_THEME, "northern-lights");
        assert_eq!(DEFAULT_MODE, "light");
    }

    #[test]
    fn mode_accepte_uniquement_light_et_dark() {
        assert!(validate("vercel", "light").is_ok());
        assert!(validate("vercel", "dark").is_ok());
        // Pas de mode `system` : le defaut est explicite.
        assert!(validate("vercel", "system").is_err());
        assert!(validate("vercel", "").is_err());
    }

    #[test]
    fn le_coeur_valide_la_forme_du_nom_sans_connaitre_la_liste_des_presets() {
        // Un preset inconnu du coeur mais bien forme est accepte : la liste
        // des 42 presets vit dans la SPA, jamais ici.
        assert!(validate("un-preset-ajoute-plus-tard", "light").is_ok());
        // Formes refusees : vide, trop long, caracteres hors [a-z0-9-].
        assert!(validate("", "light").is_err());
        assert!(validate(&"a".repeat(65), "light").is_err());
        assert!(validate("Vercel", "light").is_err());
        assert!(validate("v e r c e l", "light").is_err());
        assert!(validate("../../etc/passwd", "light").is_err());
    }
}
```

Dans `crates/ritornello-core/src/state.rs`, ajouter au `mod tests` :

```rust
    #[test]
    fn theme_et_mode_absents_par_defaut_et_roundtrip() {
        assert_eq!(PersistedState::default().theme, None);
        assert_eq!(PersistedState::default().mode, None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let st = PersistedState {
            active_source: "radio".into(),
            volume: 50,
            audio_device: None,
            locale: None,
            theme: Some("cyberpunk".into()),
            mode: Some("dark".into()),
        };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
    }

    #[test]
    fn un_state_json_anterieur_reste_lisible() {
        // Compatibilite ascendante : un fichier ecrit avant cette version n'a
        // ni `theme` ni `mode` ; il doit se charger sans erreur.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"audio_device":null,"locale":"fr"}"#,
        )
        .unwrap();
        let st = load(&path);
        assert_eq!(st.volume, 42);
        assert_eq!(st.locale.as_deref(), Some("fr"));
        assert_eq!(st.theme, None);
        assert_eq!(st.mode, None);
    }
```

Dans `crates/ritornello-core/src/status.rs`, ajouter au `mod tests` :

```rust
    #[tokio::test]
    async fn get_theme_renvoie_les_defauts_quand_rien_nest_persiste() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/theme").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["theme"], "northern-lights");
        assert_eq!(v["mode"], "light");
    }

    #[tokio::test]
    async fn put_theme_notifie_et_met_a_jour_la_selection() {
        let (state, mut theme_rx) = app_state_with_theme();
        let theme_current = state.theme_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/theme")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"theme":"cyberpunk","mode":"dark"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let recu = theme_rx.recv().await.unwrap();
        assert_eq!(recu.theme, "cyberpunk");
        assert_eq!(recu.mode, "dark");
        assert_eq!(theme_current.read().await.theme, "cyberpunk");
    }

    #[tokio::test]
    async fn put_theme_invalide_renvoie_422_et_ne_change_rien() {
        let (state, mut theme_rx) = app_state_with_theme();
        let theme_current = state.theme_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/theme")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"theme":"cyberpunk","mode":"system"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(theme_current.read().await.theme, "northern-lights");
        assert!(theme_rx.try_recv().is_err(), "rien ne doit partir dans le canal");
    }
```

et le constructeur d'état correspondant, à côté des existants :

```rust
    /// Variante avec un `theme_tx` observable, pour les tests de `/api/theme`.
    fn app_state_with_theme() -> (AppState, tokio::sync::mpsc::Receiver<crate::theme::ThemeState>) {
        let (state, _audio_rx) = app_state_with_audio();
        let (theme_tx, theme_rx) = tokio::sync::mpsc::channel(4);
        (AppState { theme_tx, ..state }, theme_rx)
    }
```

- [ ] **Step 2: Lancer les tests — échec attendu**

Run : `cargo test -p ritornello-core`
Expected : ÉCHEC de compilation — `module theme not found`, champs `theme` /
`mode` inconnus de `PersistedState`, `theme_current` inconnu d'`AppState`.

- [ ] **Step 3: Étendre l'état persisté**

Dans `crates/ritornello-core/src/state.rs`, ajouter à `PersistedState` :

```rust
    /// Preset de thème choisi (nom opaque pour le cœur : la liste des presets
    /// vit dans la SPA). Absent = `theme::DEFAULT_THEME`.
    #[serde(default)]
    pub theme: Option<String>,
    /// `"light"` ou `"dark"`. Absent = `theme::DEFAULT_MODE`.
    #[serde(default)]
    pub mode: Option<String>,
```

et à `Default::default()` : `theme: None, mode: None`.

- [ ] **Step 4: Écrire le module `theme.rs`**

Au-dessus du `mod tests` déjà écrit :

```rust
use crate::status::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Preset par défaut de l'installation. Le cœur n'en connaît que le nom.
pub const DEFAULT_THEME: &str = "northern-lights";
/// Mode par défaut. Il n'existe **pas** de mode `system` : le défaut est
/// explicite et persisté, comme la locale.
pub const DEFAULT_MODE: &str = "light";

const MAX_NOM: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeState {
    pub theme: String,
    pub mode: String,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self { theme: DEFAULT_THEME.to_string(), mode: DEFAULT_MODE.to_string() }
    }
}

/// Valide la **forme** seulement : le cœur ne connaît pas la liste des 42
/// presets (elle vit dans la SPA) et ne peut donc pas vérifier l'existence du
/// preset demandé. Il vérifie en revanche que le nom est un identifiant
/// plausible — ce qui écarte au passage les valeurs qui n'auraient rien à
/// faire dans un fichier d'état ou dans une page HTML.
pub fn validate(theme: &str, mode: &str) -> Result<(), String> {
    if mode != "light" && mode != "dark" {
        return Err(format!("mode inconnu: {mode}"));
    }
    if theme.is_empty() || theme.len() > MAX_NOM {
        return Err("nom de theme de longueur invalide".to_string());
    }
    if !theme.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("nom de theme hors [a-z0-9-]".to_string());
    }
    Ok(())
}

pub async fn theme_json(State(state): State<AppState>) -> Json<ThemeState> {
    Json(state.theme_current.read().await.clone())
}

pub async fn theme_put(State(state): State<AppState>, Json(req): Json<ThemeState>) -> Response {
    if let Err(msg) = validate(&req.theme, &req.mode) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    *state.theme_current.write().await = req.clone();
    if state.theme_tx.send(req).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
```

- [ ] **Step 5: Câbler l'état et les routes**

Dans `crates/ritornello-core/src/status.rs`, ajouter à `AppState` :

```rust
    pub theme_current: Arc<RwLock<crate::theme::ThemeState>>,
    pub theme_tx: mpsc::Sender<crate::theme::ThemeState>,
```

et deux routes à `router()` :

```rust
        .route("/api/theme", get(crate::theme::theme_json).put(crate::theme::theme_put))
```

Renseigner les deux nouveaux champs dans **les cinq** constructeurs d'état de
test qui existent : `app_state()`, `app_state_with_audio()`,
`app_state_with_cmd()`, `app_state_fr()` (dans `status.rs`) et `state_with()`
(dans `admin.rs`) — chacun avec
`theme_current: Arc::new(tokio::sync::RwLock::new(Default::default()))` et un
`theme_tx` issu d'un `mpsc::channel(4)` dont le récepteur est ignoré.

Dans `crates/ritornello-core/src/main.rs`, déclarer `mod theme;`, créer le
canal à côté des autres :

```rust
    let (theme_tx, mut theme_rx) = mpsc::channel::<theme::ThemeState>(4);
```

construire l'état courant depuis le fichier persisté :

```rust
    let theme_current = Arc::new(RwLock::new(theme::ThemeState {
        theme: persisted.theme.clone().unwrap_or_else(|| theme::DEFAULT_THEME.to_string()),
        mode: persisted.mode.clone().unwrap_or_else(|| theme::DEFAULT_MODE.to_string()),
    }));
```

le passer à `AppState` (`theme_current: theme_current.clone(), theme_tx: theme_tx.clone()`),
et ajouter un bras à la boucle `select!`, juste après celui de `locale_rx` :

```rust
            Some(t) = theme_rx.recv() => {
                core.set_theme(t);
            }
```

- [ ] **Step 6: Persister depuis le cœur**

Dans `crates/ritornello-core/src/core.rs` : ajouter les champs
`theme: Option<String>` et `mode: Option<String>` à `Core`, les initialiser
dans `new` depuis `persisted.theme.clone()` / `persisted.mode.clone()`, les
reporter dans `persist()` :

```rust
            theme: self.theme.clone(),
            mode: self.mode.clone(),
```

et ajouter la méthode :

```rust
    /// Change le thème courant et le persiste. Contrairement à `set_locale`,
    /// rien n'est poussé aux plugins : le thème est un réglage d'apparence de
    /// l'IHM web, dont aucun plugin n'a connaissance.
    ///
    /// Appelée depuis la boucle `select!` de `main` sur réception du canal
    /// `theme_rx`, lui-même alimenté par la route `PUT /api/theme`.
    pub fn set_theme(&mut self, t: crate::theme::ThemeState) {
        self.theme = Some(t.theme);
        self.mode = Some(t.mode);
        self.persist();
    }
```

- [ ] **Step 7: Lancer les tests — succès attendu**

Run : `cargo test -p ritornello-core && cargo clippy -p ritornello-core --all-targets -- -D warnings`
Expected : SUCCÈS, dont les 3 tests de `theme.rs`, les 2 de `state.rs` et les
3 de `status.rs` ajoutés plus haut.

- [ ] **Step 8: Commit**

```bash
git add crates/ritornello-core/src
git commit -m "feat(core): theme de l'appareil (GET/PUT /api/theme) persiste dans state.json"
```

---

### Task 6: Embarquement de la SPA dans le cœur — `rust-embed`, bouchon `build.rs`, routes `/` et `/assets/*`

Le cœur sert la SPA. C'est ici que se joue la compatibilité avec `cross` : le
livrable npm est lu **à la compilation**, et un bouchon garantit qu'un
`cargo build` sans Node reste vert.

**Files:**
- Create: `crates/ritornello-core/build.rs`, `crates/ritornello-core/src/placeholder.rs`, `crates/ritornello-core/src/web.rs`
- Modify: `crates/ritornello-core/Cargo.toml`, `Cargo.lock`, `crates/ritornello-core/src/main.rs`, `src/status.rs`

**Interfaces:**
- Consumes: `AppState`, `theme::ThemeState` (Task 5) ; le `dist/` produit en Task 4.
- Produces:
  - `pub fn placeholder_html(commande: &str) -> String` (dans `placeholder.rs`, inclus **textuellement** par `build.rs`)
  - `pub fn inject_theme(html: &str, theme: &str, mode: &str) -> String`
  - `pub fn serves_shell(path: &str) -> bool`
  - `pub fn mime_for(path: &str) -> &'static str`
  - `pub fn cache_control(path: &str) -> &'static str`
  - `pub fn routes() -> Router<AppState>` (route `/assets/*chemin`) et `pub async fn shell(State<AppState>, Uri, HeaderMap) -> Response` (le repli)

- [ ] **Step 1: Ajouter la dépendance**

Dans `crates/ritornello-core/Cargo.toml` :

```toml
rust-embed = { version = "8", features = ["debug-embed"] }
```

`debug-embed` est **nécessaire** : sans elle, `rust-embed` relit les fichiers
depuis le disque en profil debug, et les tests dépendraient alors de la
présence d'un `dist/` construit au moment de leur exécution. Avec elle, le
comportement est identique en debug et en release — le contenu est figé à la
compilation, ce qui est précisément la propriété qu'on veut vérifier.

- [ ] **Step 2: Écrire les tests (doivent échouer : les modules n'existent pas)**

`crates/ritornello-core/src/placeholder.rs` :

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_bouchon_est_un_html_qui_invite_a_construire_lihm() {
        let html = placeholder_html("npm run build --workspaces");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("npm run build --workspaces"));
        // Pas de faux positif : le bouchon doit se reconnaitre a coup sur.
        assert!(html.contains(MARQUEUR));
    }
}
```

`crates/ritornello-core/src/web.rs` :

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    #[test]
    fn inject_theme_pose_le_choix_avant_la_fermeture_du_head() {
        let html = "<!doctype html><html><head><title>x</title></head><body></body></html>";
        let out = inject_theme(html, "cyberpunk", "dark");
        assert!(out.contains(r#"window.__RITORNELLO_THEME__={"mode":"dark","theme":"cyberpunk"}"#));
        assert!(out.find("__RITORNELLO_THEME__").unwrap() < out.find("</head>").unwrap());
    }

    #[test]
    fn inject_theme_survit_a_un_html_sans_head() {
        let out = inject_theme("<div>x</div>", "vercel", "light");
        assert!(out.contains("__RITORNELLO_THEME__"));
    }

    #[test]
    fn le_repli_ne_sert_le_shell_que_hors_des_espaces_de_donnees() {
        // Les URL historiques et les routes du routeur Vue : shell.
        assert!(serves_shell("/"));
        assert!(serves_shell("/status"));
        assert!(serves_shell("/plugins/radio/"));
        // Les espaces de donnees : jamais de shell, sinon une faute de frappe
        // sur une route d'API repondrait 200 avec du HTML — piege a debogage.
        assert!(!serves_shell("/api/statuss"));
        assert!(!serves_shell("/api/theme"));
        assert!(!serves_shell("/assets/inconnu.js"));
        assert!(!serves_shell("/plugins/radio/api/data"));
        assert!(!serves_shell("/plugins/radio/ui.js"));
        assert!(!serves_shell("/plugins/radio/ui.css"));
    }

    #[test]
    fn mime_et_cache_selon_le_nom_du_fichier() {
        assert_eq!(mime_for("app-abc.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app-abc.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("inconnu.bin"), "application/octet-stream");
        // Nom hashe : immuable. Noms stables du contrat : a revalider.
        assert!(cache_control("app-abc123.js").contains("immutable"));
        assert!(!cache_control("vue.js").contains("immutable"));
        assert!(!cache_control("ui-kit.js").contains("immutable"));
    }

    #[tokio::test]
    async fn la_racine_sert_le_shell_avec_le_theme_injecte() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app.oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(
            resp.into_body().collect().await.unwrap().to_bytes().to_vec(),
        )
        .unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
        assert!(html.contains("northern-lights"));
    }

    #[tokio::test]
    async fn un_chemin_inconnu_hors_api_sert_le_shell() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp =
            app.oneshot(Request::get("/plugins/quelconque/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn une_route_dapi_inconnue_repond_404_et_non_le_shell() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app.oneshot(Request::get("/api/statuss").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn le_contenu_embarque_est_servable() {
        // Vrai `dist/` ou bouchon : l'un des deux est necessairement present,
        // `build.rs` le garantit.
        let html = shell_html();
        assert!(!html.is_empty());
        assert!(html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"));
    }

    #[tokio::test]
    async fn un_actif_absent_repond_404() {
        let app = crate::status::router(crate::status::tests_support::app_state());
        let resp = app
            .oneshot(Request::get("/assets/nexiste-pas.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
```

Ces tests réutilisent les constructeurs d'état des tests de `status.rs` : les
extraire dans un module `pub(crate) mod tests_support` de `status.rs`, gardé
par `#[cfg(test)]`, et faire pointer les tests existants dessus (déplacement
mécanique, aucun changement de contenu).

- [ ] **Step 3: Lancer les tests — échec attendu**

Run : `cargo test -p ritornello-core`
Expected : ÉCHEC de compilation — `placeholder_html`, `inject_theme`,
`serves_shell`, `mime_for`, `cache_control`, `shell_html` introuvables.

- [ ] **Step 4: Écrire le bouchon et le `build.rs`**

`crates/ritornello-core/src/placeholder.rs`, au-dessus du `mod tests` :

```rust
//! Page servie quand l'IHM n'a pas été construite.
//!
//! Ce fichier est inclus **textuellement** par `build.rs` (`include!`) autant
//! qu'il est compilé comme module du crate : c'est ce qui permet de tester la
//! fabrication du bouchon par `cargo test`, alors que Cargo n'exécute jamais
//! les tests d'un script de build. Il ne doit donc dépendre d'**aucune**
//! crate externe.

/// Marqueur reconnaissable dans la page de bouchon.
pub const MARQUEUR: &str = "ritornello-ihm-non-construite";

/// HTML minimal, sans dépendance, qui explique quoi lancer. Mieux qu'une
/// erreur de macro `include_str!` sur un clone frais : `cargo build` et
/// `cargo test` restent verts sans Node installé, et le message est explicite.
pub fn placeholder_html(commande: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>ritornello</title></head><body id=\"{MARQUEUR}\">\
         <h1>ritornello</h1>\
         <p>Web interface not built. Run:</p><pre>{commande}</pre>\
         </body></html>"
    )
}
```

`crates/ritornello-core/build.rs` :

```rust
// Garantit que le répertoire embarqué par `rust-embed` existe et contient au
// moins un `index.html`. Le build npm n'est **jamais** invoqué ici : la
// cross-compilation par `cross` tourne dans une image Docker sans Node, et le
// livrable y est déjà présent sur le disque (voir `deploy/build.sh`).
include!("src/placeholder.rs");

const DIST: &str = "../../web/app/dist";

fn main() {
    println!("cargo::rerun-if-changed={DIST}");
    println!("cargo::rerun-if-changed=src/placeholder.rs");
    let dist = std::path::Path::new(DIST);
    let index = dist.join("index.html");
    if index.exists() {
        return;
    }
    println!("cargo::warning=IHM web non construite : bouchon embarque a la place");
    std::fs::create_dir_all(dist).expect("creation de web/app/dist");
    std::fs::write(&index, placeholder_html("npm ci && npm run build --workspaces"))
        .expect("ecriture du bouchon");
}
```

- [ ] **Step 5: Écrire `web.rs`**

Au-dessus du `mod tests` :

```rust
use crate::status::AppState;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rust_embed::RustEmbed;

/// Le `dist/` produit par `npm run build --workspaces`, embarqué à la
/// compilation. `build.rs` garantit qu'il existe (bouchon à défaut).
#[derive(RustEmbed)]
#[folder = "../../web/app/dist/"]
struct Dist;

pub fn mime_for(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

/// Les chunks de l'app portent un hash dans leur nom : ils sont immuables.
/// `vue.js` et `ui-kit.js` gardent au contraire un **nom stable** — c'est le
/// contrat que les modules de plugin importent — donc ils doivent être
/// revalidés (l'`ETag` s'en charge).
pub fn cache_control(path: &str) -> &'static str {
    let nom = path.rsplit('/').next().unwrap_or(path);
    if nom.starts_with("app-") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Le repli ne sert le shell **que** hors des espaces de données. Sans cette
/// restriction, une faute de frappe sur une route d'API répondrait 200 avec du
/// HTML — un piège à débogage coûteux.
pub fn serves_shell(path: &str) -> bool {
    if path.starts_with("/api/") || path.starts_with("/assets/") {
        return false;
    }
    if let Some(reste) = path.strip_prefix("/plugins/") {
        if let Some((_, apres)) = reste.split_once('/') {
            if apres.starts_with("api/") || apres.starts_with("ui.") {
                return false;
            }
        }
    }
    true
}

pub fn inject_theme(html: &str, theme: &str, mode: &str) -> String {
    // `serde_json` échappe la valeur : rien de ce qui vient de `state.json` ne
    // peut casser le script injecté.
    let payload = serde_json::json!({ "theme": theme, "mode": mode });
    let script = format!("<script>window.__RITORNELLO_THEME__={payload};</script>");
    match html.find("</head>") {
        Some(i) => format!("{}{}{}", &html[..i], script, &html[i..]),
        None => format!("{script}{html}"),
    }
}

/// Le shell embarqué, ou le bouchon si `build.rs` n'a trouvé aucun livrable.
pub fn shell_html() -> String {
    match Dist::get("index.html") {
        Some(f) => String::from_utf8_lossy(&f.data).into_owned(),
        None => crate::placeholder::placeholder_html("npm ci && npm run build --workspaces"),
    }
}

fn etag_of(hash: &[u8]) -> String {
    let hex: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

async fn asset(headers: HeaderMap, uri: Uri) -> Response {
    let chemin = uri.path().trim_start_matches("/assets/");
    let Some(f) = Dist::get(&format!("assets/{chemin}")) else {
        return (StatusCode::NOT_FOUND, "actif inconnu").into_response();
    };
    let etag = etag_of(&f.metadata.sha256_hash());
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime_for(chemin)),
            (header::CACHE_CONTROL, cache_control(chemin)),
            (header::ETAG, etag.as_str()),
        ],
        f.data.to_vec(),
    )
        .into_response()
}

/// Repli du routeur : sert le shell pour les chemins de la SPA, 404 sinon.
pub async fn shell(State(state): State<AppState>, uri: Uri) -> Response {
    if !serves_shell(uri.path()) {
        return (StatusCode::NOT_FOUND, "inconnu").into_response();
    }
    let t = state.theme_current.read().await.clone();
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        inject_theme(&shell_html(), &t.theme, &t.mode),
    )
        .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/assets/*chemin", get(asset))
}
```

- [ ] **Step 6: Câbler dans le routeur**

Dans `crates/ritornello-core/src/status.rs`, à la fin de `router()` :

```rust
        .merge(crate::web::routes())
        .fallback(crate::web::shell)
        .with_state(state)
```

(le `.with_state(state)` existant reste **le dernier** appel.)

Dans `main.rs`, déclarer les deux modules : `mod placeholder;` et `mod web;`,
et remplacer le message de démarrage `page de statut sur http://…/status` par
`interface web sur http://{http_addr}/`.

- [ ] **Step 7: Lancer les tests — succès attendu**

Run : `npm run build --workspaces && cargo test -p ritornello-core && cargo clippy -p ritornello-core --all-targets -- -D warnings`
Expected : SUCCÈS.

Vérifier ensuite le chemin du bouchon, qui est la garantie que `cross`
fonctionnera sans Node :

```bash
mv web/app/dist /tmp/dist-sauvegarde
cargo build -p ritornello-core 2>&1 | grep -q "bouchon" && echo "bouchon OK"
cargo test -p ritornello-core   # doit rester vert
mv /tmp/dist-sauvegarde web/app/dist
touch crates/ritornello-core/build.rs && cargo build -p ritornello-core
```

- [ ] **Step 8: Commit**

```bash
git add crates/ritornello-core Cargo.lock
git commit -m "feat(core): SPA embarquee (rust-embed), routes / et /assets, bouchon si l'IHM n'est pas construite"
```

---

### Task 7: Catalogues i18n en JSON — `Catalog::entries()` et `GET /api/i18n`

Ouvre la voie de sortie du mécanisme `{{clé}}` : les catalogues deviennent des
données. Côté cœur seulement ; les plugins suivent en Task 10.

**Files:**
- Modify: `crates/ritornello-i18n/src/lib.rs`, `crates/ritornello-core/src/status.rs`

**Interfaces:**
- Consumes: `Catalog` (existant).
- Produces:
  - `pub fn entries(&self) -> std::collections::HashMap<&str, &str>` sur `Catalog`
  - Route `GET /api/i18n` → objet plat `{ "<clé>": "<texte>" }`

- [ ] **Step 1: Écrire les tests (doivent échouer)**

Dans `crates/ritornello-i18n/src/lib.rs`, `mod tests` :

```rust
    #[test]
    fn entries_fusionne_own_par_dessus_common() {
        let dir = tempfile::tempdir().unwrap();
        // `error` existe dans le common embarque : `own` doit primer, comme
        // dans `get`.
        let cat = Catalog::load("core", "en", dir.path(), "error = \"own-error\"\nautre = \"x\"\n");
        let e = cat.entries();
        assert_eq!(e.get("error").copied(), Some("own-error"));
        assert_eq!(e.get("autre").copied(), Some("x"));
        // Les cles du common non redefinies sont presentes : la carte est
        // complete, c'est elle qui alimente `t()` cote navigateur.
        assert!(e.len() > 1);
        assert!(e.keys().any(|k| *k == "play"), "le vocabulaire commun doit etre inclus");
    }

    #[test]
    fn entries_reflete_les_surcharges_externes() {
        let dir = tempfile::tempdir().unwrap();
        ecrire(dir.path(), "core", "fr.toml", "standby = \"VEILLE\"\n");
        let cat = Catalog::load("core", "fr", dir.path(), "standby = \"STANDBY\"\n");
        assert_eq!(cat.entries().get("standby").copied(), Some("VEILLE"));
    }
```

Dans `crates/ritornello-core/src/status.rs`, `mod tests` :

```rust
    #[tokio::test]
    async fn api_i18n_renvoie_le_catalogue_a_plat() {
        let app = router(tests_support::app_state());
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // L'anglais embarque du coeur porte ces cles (src/locales/en.toml).
        assert!(v["remote_title"].is_string());
        assert!(v["audio_output"].is_string());
    }

    #[tokio::test]
    async fn api_i18n_suit_la_langue_courante() {
        let (state, _rx, _dir) = tests_support::app_state_fr();
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/i18n").body(Body::empty()).unwrap()).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["audio_output"], "Sortie audio");
    }
```

- [ ] **Step 2: Lancer les tests — échec attendu**

Run : `cargo test -p ritornello-i18n -p ritornello-core`
Expected : ÉCHEC — `no method named entries`, route `/api/i18n` absente (404).

- [ ] **Step 3: Implémenter `entries()`**

Dans `impl Catalog`, à côté de `get` :

```rust
    /// Carte plate de **toutes** les clés connues, `own` surchargeant
    /// `common` — même ordre de priorité que `get`, mais exposé d'un bloc.
    ///
    /// Sert à livrer le catalogue au navigateur (`GET /api/i18n`) : la SPA
    /// résout ses clés côté client, ce qui remplace la substitution `{{clé}}`
    /// d'autrefois. Les valeurs restent des **données** de bout en bout :
    /// aucun caractère n'est dangereux, contrairement à la substitution brute
    /// dans du source JS.
    pub fn entries(&self) -> std::collections::HashMap<&str, &str> {
        let mut out: std::collections::HashMap<&str, &str> =
            self.common.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        for (k, v) in &self.own {
            out.insert(k.as_str(), v.as_str());
        }
        out
    }
```

- [ ] **Step 4: Ajouter la route**

Dans `status.rs` :

```rust
        .route("/api/i18n", get(i18n_json))
```

```rust
/// Catalogue du cœur dans la langue courante, à plat, pour le `t()` de la SPA.
async fn i18n_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cat = state.catalog.read().await;
    Json(serde_json::json!(cat.entries()))
}
```

- [ ] **Step 5: Lancer les tests — succès attendu**

Run : `cargo test -p ritornello-i18n -p ritornello-core && cargo clippy -p ritornello-i18n -p ritornello-core --all-targets -- -D warnings`
Expected : SUCCÈS.

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-i18n crates/ritornello-core/src/status.rs
git commit -m "feat(i18n): export du catalogue a plat et route GET /api/i18n"
```

---

### Task 8: En-tête — bascule clair/sombre et popin de sélection des 42 thèmes

La fonctionnalité demandée arrive à l'écran. Le moteur (Task 2) et la route
(Task 5) existent : il reste l'état partagé et les deux composants.

**Files:**
- Create: `web/app/src/composables/useTheme.ts`, `useTheme.test.ts`
- Create: `web/app/src/components/ThemeToggle.vue`, `ThemePicker.vue`, `ThemePicker.test.ts`
- Modify: `web/app/src/App.vue`, `src/main.ts`

**Interfaces:**
- Consumes: `applyTheme`, `presets`, `resolveVars`, `api`, `toast`, composants `Dialog*`, `Button`, `Input` (Task 3) ; `readBootTheme` (Task 4) ; `PUT /api/theme` (Task 5).
- Produces:
  - `export function initTheme(): void` — appelée par `main.ts` avant le montage
  - `export function useTheme(): { theme: Ref<string>; mode: Ref<Mode>; set(next: Partial<ThemePayload>): Promise<void>; toggleMode(): Promise<void> }`
  - `export function filterPresets(query: string): Array<{ id: string; label: string }>`

- [ ] **Step 0: Installer la dépendance d'icônes du shell**

Les deux boutons de l'en-tête utilisent des icônes ; `lucide-vue-next` est une
dépendance du kit, pas encore du shell :

```bash
npm i -w app lucide-vue-next
```

- [ ] **Step 1: Écrire les tests (doivent échouer)**

`web/app/src/composables/useTheme.test.ts` :

```ts
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { initTheme, useTheme } from './useTheme'

function mockFetch() {
  const spy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
  vi.stubGlobal('fetch', spy)
  return spy
}

describe('useTheme', () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    document.documentElement.removeAttribute('style')
    document.documentElement.className = ''
    window.__RITORNELLO_THEME__ = { theme: 'northern-lights', mode: 'light' }
    initTheme()
  })

  it('part du choix injecté et applique les variables', () => {
    const { theme, mode } = useTheme()
    expect(theme.value).toBe('northern-lights')
    expect(mode.value).toBe('light')
    expect(document.documentElement.style.getPropertyValue('--primary')).toBe('#34a85a')
  })

  it('toggleMode bascule le mode, applique et persiste', async () => {
    const spy = mockFetch()
    const { mode, toggleMode } = useTheme()
    await toggleMode()
    expect(mode.value).toBe('dark')
    expect(document.documentElement.classList.contains('dark')).toBe(true)
    expect(spy).toHaveBeenCalledWith(
      '/api/theme',
      expect.objectContaining({
        method: 'PUT',
        body: JSON.stringify({ theme: 'northern-lights', mode: 'dark' }),
      }),
    )
    await toggleMode()
    expect(mode.value).toBe('light')
  })

  it('changer de preset conserve le mode courant', async () => {
    mockFetch()
    const { set, theme, mode } = useTheme()
    await set({ mode: 'dark' })
    await set({ theme: 'vercel' })
    expect(theme.value).toBe('vercel')
    expect(mode.value).toBe('dark')
  })

  it('applique le choix localement même si la persistance échoue', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: 'boum' }), { status: 422 })),
    )
    const { set, theme } = useTheme()
    await set({ theme: 'cyberpunk' })
    // Le choix reste visible : refuser d'appliquer donnerait une IHM figée
    // sans explication.
    expect(theme.value).toBe('cyberpunk')
  })
})
```

`web/app/src/components/ThemePicker.test.ts` :

```ts
import { presets } from '@ritornello/ui'
import { mount } from '@vue/test-utils'
import { describe, expect, it } from 'vitest'
import { filterPresets } from '../composables/useTheme'
import ThemePicker from './ThemePicker.vue'

describe('filterPresets', () => {
  it('sans filtre, renvoie les 42 presets', () => {
    expect(filterPresets('')).toHaveLength(Object.keys(presets).length)
    expect(filterPresets('')).toHaveLength(42)
  })

  it('filtre sur le libellé, insensible à la casse et aux espaces', () => {
    const r = filterPresets('  NORTHERN ')
    expect(r).toHaveLength(1)
    expect(r[0]?.id).toBe('northern-lights')
  })

  it('filtre aussi sur l’identifiant', () => {
    expect(filterPresets('northern-lights')[0]?.label).toBe('Northern Lights')
  })

  it('renvoie une liste vide sur un filtre sans correspondance', () => {
    expect(filterPresets('zzzzz')).toEqual([])
  })
})

describe('ThemePicker', () => {
  it('liste les 42 thèmes avec quatre pastilles chacun', () => {
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    expect(w.findAll('[data-preset]')).toHaveLength(42)
    const carte = w.find('[data-preset="northern-lights"]')
    expect(carte.findAll('[data-swatch]')).toHaveLength(4)
  })

  it('marque le thème actif', () => {
    const w = mount(ThemePicker, { props: { current: 'vercel', mode: 'light' } })
    expect(w.find('[data-preset="vercel"]').attributes('data-active')).toBe('true')
    expect(w.find('[data-preset="northern-lights"]').attributes('data-active')).toBe('false')
  })

  it('émet le preset choisi', async () => {
    const w = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    await w.find('[data-preset="vercel"]').trigger('click')
    expect(w.emitted('choose')).toEqual([['vercel']])
  })

  it('les pastilles suivent le mode affiché', () => {
    const clair = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'light' } })
    const sombre = mount(ThemePicker, { props: { current: 'northern-lights', mode: 'dark' } })
    const fond = (w: ReturnType<typeof mount>) =>
      w.find('[data-preset="northern-lights"] [data-swatch="background"]').attributes('style')
    expect(fond(clair)).toContain('rgb(249, 249, 250)')
    expect(fond(sombre)).toContain('rgb(26, 29, 35)')
  })
})
```

- [ ] **Step 2: Lancer les tests — échec attendu**

Run : `npm test -w app`
Expected : ÉCHEC — `./useTheme` et `./ThemePicker.vue` introuvables.

- [ ] **Step 3: Écrire le composable**

`web/app/src/composables/useTheme.ts` :

```ts
import { api, applyTheme, DEFAULT_MODE, DEFAULT_PRESET, presets, toast, type Mode } from '@ritornello/ui'
import { ref } from 'vue'
import { readBootTheme } from '../boot'
import type { ThemePayload } from '../types'

// État au niveau du module : le thème est unique pour la page, un singleton
// est plus simple qu'un `provide`/`inject` traversé par tous les composants.
const theme = ref(DEFAULT_PRESET)
const mode = ref<Mode>(DEFAULT_MODE)

/// Appelée par `main.ts` **avant** le montage : le premier rendu est déjà dans
/// les bonnes couleurs (aucun clignotement).
export function initTheme(): void {
  const choix = readBootTheme()
  theme.value = choix.theme
  mode.value = choix.mode
  applyTheme(choix.theme, choix.mode)
}

export function useTheme() {
  /// Applique **d'abord**, persiste ensuite : le réglage est un choix
  /// d'apparence, l'utilisateur doit le voir immédiatement. Si la persistance
  /// échoue, on le signale sans revenir en arrière — annuler silencieusement
  /// donnerait une IHM qui semble ignorer les clics.
  async function set(next: Partial<ThemePayload>): Promise<void> {
    const t = next.theme ?? theme.value
    const m = next.mode ?? mode.value
    applyTheme(t, m)
    theme.value = t
    mode.value = m
    const err = await api.put('/api/theme', { theme: t, mode: m })
    if (err) toast.error(err)
  }

  return {
    theme,
    mode,
    set,
    toggleMode: () => set({ mode: mode.value === 'dark' ? 'light' : 'dark' }),
  }
}

/// Filtre par libellé **ou** identifiant, insensible à la casse. Trié par
/// libellé pour que la grille de la popin soit stable et parcourable.
export function filterPresets(query: string): Array<{ id: string; label: string }> {
  const q = query.trim().toLowerCase()
  return Object.entries(presets)
    .map(([id, p]) => ({ id, label: p.label }))
    .filter(({ id, label }) => !q || label.toLowerCase().includes(q) || id.includes(q))
    .sort((a, b) => a.label.localeCompare(b.label))
}
```

- [ ] **Step 4: Écrire les deux composants**

`web/app/src/components/ThemePicker.vue` :

```vue
<script setup lang="ts">
import { presets, resolveVars, type Mode } from '@ritornello/ui'
import { computed, ref } from 'vue'
import { filterPresets } from '../composables/useTheme'

const props = defineProps<{ current: string; mode: Mode }>()
defineEmits<{ choose: [id: string] }>()

const query = ref('')
const liste = computed(() => filterPresets(query.value))

// Les quatre pastilles rendues dans le mode affiche : un preset se reconnait
// bien plus vite a ses couleurs qu'a son nom.
const PASTILLES = ['background', 'primary', 'secondary', 'accent'] as const

function couleur(id: string, cle: string): string {
  const p = presets[id]
  return p ? (resolveVars(p, props.mode)[cle] ?? 'transparent') : 'transparent'
}
</script>

<template>
  <div class="space-y-3">
    <input
      v-model="query"
      class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
      placeholder="filter"
    />
    <div class="grid max-h-[60vh] grid-cols-2 gap-2 overflow-y-auto sm:grid-cols-3">
      <button
        v-for="p in liste"
        :key="p.id"
        :data-preset="p.id"
        :data-active="String(p.id === props.current)"
        class="flex flex-col gap-2 rounded-md border p-2 text-left text-sm"
        :class="p.id === props.current ? 'border-primary ring-1 ring-primary' : 'border-border'"
        @click="$emit('choose', p.id)"
      >
        <span class="truncate">{{ p.label }}</span>
        <span class="flex gap-1">
          <span
            v-for="cle in PASTILLES"
            :key="cle"
            :data-swatch="cle"
            class="h-4 w-4 rounded-full border border-border"
            :style="{ background: couleur(p.id, cle) }"
          />
        </span>
      </button>
    </div>
    <p v-if="!liste.length" class="text-sm text-muted-foreground">—</p>
  </div>
</template>
```

`web/app/src/components/ThemeToggle.vue` :

```vue
<script setup lang="ts">
import { Button, Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from '@ritornello/ui'
import { Moon, Palette, Sun } from 'lucide-vue-next'
import { ref } from 'vue'
import { useTheme } from '../composables/useTheme'
import ThemePicker from './ThemePicker.vue'

const { theme, mode, set, toggleMode } = useTheme()
const ouvert = ref(false)

async function choisir(id: string) {
  await set({ theme: id })
  ouvert.value = false
}
</script>

<template>
  <div class="flex items-center gap-1">
    <Button variant="ghost" size="icon" aria-label="toggle theme mode" @click="toggleMode()">
      <Sun v-if="mode === 'dark'" class="size-4" />
      <Moon v-else class="size-4" />
    </Button>
    <Dialog v-model:open="ouvert">
      <DialogTrigger as-child>
        <Button variant="ghost" size="icon" aria-label="pick theme">
          <Palette class="size-4" />
        </Button>
      </DialogTrigger>
      <DialogContent class="sm:max-w-2xl">
        <DialogHeader><DialogTitle>Theme</DialogTitle></DialogHeader>
        <ThemePicker :current="theme" :mode="mode" @choose="choisir" />
      </DialogContent>
    </Dialog>
  </div>
</template>
```

- [ ] **Step 5: Brancher dans l'en-tête et l'amorçage**

Dans `web/app/src/App.vue` : importer `ThemeToggle` et remplacer
`<span class="ml-auto" />` par `<ThemeToggle class="ml-auto" />`.

Dans `web/app/src/main.ts` : remplacer les deux lignes
`const { theme, mode } = readBootTheme()` / `applyTheme(theme, mode)` par
`initTheme()` (importé de `./composables/useTheme`), et retirer les imports
devenus inutiles (`applyTheme`, `readBootTheme`).

- [ ] **Step 6: Lancer les tests — succès attendu**

Run : `npm test -w app`
Expected : SUCCÈS — 4 tests de `useTheme`, 4 de `filterPresets`, 4 de
`ThemePicker`, plus ceux des tasks précédentes.

- [ ] **Step 7: Commit**

```bash
git add web/app/src
git commit -m "feat(web): bascule clair/sombre et popin de selection des 42 themes"
```

---

### Task 9: Vues Accueil et Statut, route `/api/logs`, retrait de la page HTML du cœur

Les deux surfaces du cœur passent en Vue et le HTML généré par `format!`
disparaît. Un manque de la conception apparaît ici et est comblé : les
dernières lignes de log n'étaient accessibles que par la page rendue côté
serveur — il faut désormais une route.

**Files:**
- Modify: `crates/ritornello-core/src/status.rs` (ajout de `/api/logs` ; **suppression** de `status_page`, `escape_html`, de la route `/status` et des tests de rendu HTML)
- Create: `web/app/src/views/HomeView.vue`, `web/app/src/views/StatusView.vue` (remplacent les coquilles), `web/app/src/composables/useCatalog.ts`, `src/views/HomeView.test.ts`
- Modify: `web/app/src/App.vue`, `web/app/src/types.ts`

**Interfaces:**
- Consumes: `/api/status`, `/api/audio-output`, `/api/locale`, `/api/command` (existants) ; `/api/i18n` (Task 7) ; kit et `api` (Tasks 1 et 3).
- Produces:
  - Route `GET /api/logs` → `{ "lines": ["…"] }`, les plus récentes **en premier** (l'ancienne page les affichait déjà en ordre inverse)
  - `export const REMOTE_COMMANDS: Array<{ key: string; cmd: Command }>` — les 12 commandes simples, dans l'ordre de l'ancienne page
  - `export function useCatalog(): { t: Ref<(k: string, p?: Record<string, string | number>) => string>; reload(): Promise<void> }`
  - `LogsPayload` dans `src/types.ts`

- [ ] **Step 1: Écrire les tests Rust (doivent échouer) et retirer ceux du rendu HTML**

Dans `crates/ritornello-core/src/status.rs`, `mod tests` — **supprimer** les
quatre tests qui vérifiaient le HTML rendu, devenus sans objet :
`page_statut_rendue_en_francais`, `page_statut_affiche_la_telecommande`,
`page_statut_affiche_les_dernieres_erreurs`, `page_statut_lien_admin_interne`.
Ce qu'ils couvraient est repris par les routes JSON (`/api/i18n` en Task 7,
`/api/status` déjà testée, `/api/logs` ci-dessous) et par les parcours
Playwright de la Task 13.

Ajouter :

```rust
    #[tokio::test]
    async fn api_logs_renvoie_les_lignes_les_plus_recentes_en_premier() {
        let state = tests_support::app_state();
        state.logs.push("WARN premiere".into());
        state.logs.push("WARN seconde".into());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/logs").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lignes: Vec<String> = serde_json::from_value(v["lines"].clone()).unwrap();
        // Ordre inverse, comme le faisait la page rendue cote serveur.
        assert_eq!(lignes, vec!["WARN seconde".to_string(), "WARN premiere".to_string()]);
    }

    #[tokio::test]
    async fn lancienne_route_status_est_desormais_servie_par_la_spa() {
        // `/status` reste une URL valide (README, liens existants) : elle sert
        // maintenant le shell, plus du HTML genere par le coeur.
        let app = router(tests_support::app_state());
        let resp = app.oneshot(Request::get("/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
        assert!(!html.contains("<table"), "le coeur ne genere plus de HTML metier");
    }
```

- [ ] **Step 2: Lancer les tests — échec attendu**

Run : `cargo test -p ritornello-core`
Expected : ÉCHEC — `/api/logs` répond 404, et `/status` renvoie encore la page
générée (le test cherchant `__RITORNELLO_THEME__` échoue).

- [ ] **Step 3: Ajouter `/api/logs` et retirer le rendu HTML**

Dans `status.rs` : ajouter la route et le handler…

```rust
        .route("/api/logs", get(logs_json))
```

```rust
#[derive(Serialize)]
struct LogsResponse {
    lines: Vec<String>,
}

/// Les dernières lignes WARN/ERROR, les plus récentes en premier — c'est
/// l'ordre dans lequel l'ancienne page de statut les affichait.
async fn logs_json(State(state): State<AppState>) -> Json<LogsResponse> {
    let mut lines = state.logs.snapshot();
    lines.reverse();
    Json(LogsResponse { lines })
}
```

…puis **supprimer** : la route `.route("/status", get(status_page))`, la
fonction `status_page` (~130 lignes de `format!`), la fonction `escape_html`,
et les `use axum::response::Html` devenus inutiles. Le repli du routeur
(Task 6) prend le relais sur `/status`.

- [ ] **Step 4: Écrire les tests du côté navigateur (doivent échouer)**

`web/app/src/views/HomeView.test.ts` :

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import { REMOTE_COMMANDS } from './HomeView.vue'

describe('REMOTE_COMMANDS', () => {
  it('couvre les 12 commandes simples du protocole', () => {
    expect(REMOTE_COMMANDS).toHaveLength(12)
    const cmds = REMOTE_COMMANDS.map((c) => c.cmd.cmd)
    expect(cmds).toEqual([
      'Next', 'Prev', 'VolumeUp', 'VolumeDown', 'Mute', 'PlayPause',
      'Stop', 'NextTrack', 'PrevTrack', 'Eject', 'SourceCycle', 'Power',
    ])
  })

  it('chaque commande porte une clé de traduction', () => {
    for (const c of REMOTE_COMMANDS) expect(c.key).toMatch(/^remote_/)
  })
})

describe('HomeView', () => {
  it('poste la commande Select avec le numéro de présélection', async () => {
    const spy = vi.fn().mockResolvedValue(new Response(null, { status: 204 }))
    vi.stubGlobal('fetch', spy)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await w.find('[data-preset-button="3"]').trigger('click')
    expect(spy).toHaveBeenCalledWith(
      '/api/command',
      expect.objectContaining({ method: 'POST', body: JSON.stringify({ cmd: 'Select', arg: 3 }) }),
    )
  })

  it('expose les 9 présélections de la télécommande', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    expect(w.findAll('[data-preset-button]')).toHaveLength(9)
  })
})
```

- [ ] **Step 5: Écrire le composable de catalogue et les deux vues**

`web/app/src/composables/useCatalog.ts` :

```ts
import { api, createT, type Catalog } from '@ritornello/ui'
import { computed, ref } from 'vue'

// Catalogue du coeur, partage par toutes les vues. Recharge au changement de
// langue (la page ne se recharge plus entierement comme autrefois).
const catalog = ref<Catalog>({})

export function useCatalog() {
  const t = computed(() => createT(catalog.value))
  async function reload(): Promise<void> {
    catalog.value = await api.get<Catalog>('/api/i18n').catch(() => ({}))
  }
  return { t, reload }
}
```

`web/app/src/types.ts` — ajouter :

```ts
export interface LogsPayload { lines: string[] }
export type Command = { cmd: string; arg?: number }
```

`web/app/src/views/HomeView.vue` — la télécommande, reprise à l'identique
(9 présélections + 12 commandes simples, toutes postées à `/api/command`) :

```vue
<script lang="ts">
import type { Command } from '../types'

/// Les 12 commandes simples, dans l'ordre exact de l'ancienne page de statut.
/// La charge utile est un `ritornello_proto::Command` serialise : c'est le
/// meme canal que celui alimente par les plugins Input, donc aucune logique
/// metier n'est dupliquee ici.
export const REMOTE_COMMANDS: Array<{ key: string; cmd: Command }> = [
  { key: 'remote_preset_next', cmd: { cmd: 'Next' } },
  { key: 'remote_preset_prev', cmd: { cmd: 'Prev' } },
  { key: 'remote_vol_up', cmd: { cmd: 'VolumeUp' } },
  { key: 'remote_vol_down', cmd: { cmd: 'VolumeDown' } },
  { key: 'remote_mute', cmd: { cmd: 'Mute' } },
  { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'remote_stop', cmd: { cmd: 'Stop' } },
  { key: 'remote_track_next', cmd: { cmd: 'NextTrack' } },
  { key: 'remote_track_prev', cmd: { cmd: 'PrevTrack' } },
  { key: 'remote_eject', cmd: { cmd: 'Eject' } },
  { key: 'remote_source', cmd: { cmd: 'SourceCycle' } },
  { key: 'remote_power', cmd: { cmd: 'Power' } },
]
</script>

<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import { onMounted, ref } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command, StatusPayload } from '../types'

const { t, reload } = useCatalog()
const active = ref('')

const PRESETS = [1, 2, 3, 4, 5, 6, 7, 8, 9]

onMounted(async () => {
  await reload()
  const s = await api.get<StatusPayload>('/api/status').catch(() => null)
  if (s) active.value = s.active_source
})

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}
</script>

<template>
  <div class="space-y-4">
    <p class="text-sm text-muted-foreground">
      {{ t('active_source_label') }} : <span class="text-foreground">{{ active }}</span>
    </p>
    <Card>
      <CardHeader><CardTitle>{{ t('remote_title') }}</CardTitle></CardHeader>
      <CardContent class="space-y-3">
        <div class="grid grid-cols-3 gap-2 sm:grid-cols-9">
          <Button
            v-for="n in PRESETS"
            :key="n"
            :data-preset-button="n"
            variant="secondary"
            @click="send({ cmd: 'Select', arg: n })"
          >
            {{ n }}
          </Button>
        </div>
        <div class="flex flex-wrap gap-2">
          <Button v-for="c in REMOTE_COMMANDS" :key="c.key" variant="outline" @click="send(c.cmd)">
            {{ t(c.key) }}
          </Button>
        </div>
      </CardContent>
    </Card>
  </div>
</template>
```

`web/app/src/views/StatusView.vue` — table des plugins, sortie audio, langue,
journaux :

```vue
<script setup lang="ts">
import {
  api, Badge, Button, Card, CardContent, CardHeader, CardTitle,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue, toast,
} from '@ritornello/ui'
import { onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import type { AudioPayload, LocalePayload, LogsPayload, StatusPayload } from '../types'

const { t, reload } = useCatalog()
const status = ref<StatusPayload>({ plugins: [], active_source: '' })
const audio = ref<AudioPayload>({ devices: [], current: null })
const locale = ref<LocalePayload>({ locales: [], current: null })
const logs = ref<string[]>([])
const device = ref('')
const lang = ref('')

async function chargerTout() {
  await reload()
  status.value = await api.get<StatusPayload>('/api/status').catch(() => status.value)
  audio.value = await api.get<AudioPayload>('/api/audio-output').catch(() => audio.value)
  locale.value = await api.get<LocalePayload>('/api/locale').catch(() => locale.value)
  logs.value = (await api.get<LogsPayload>('/api/logs').catch(() => ({ lines: [] }))).lines
  device.value = audio.value.current ?? ''
  lang.value = locale.value.current ?? 'en'
}

onMounted(chargerTout)

async function changerSortie() {
  const err = await api.put('/api/audio-output', { device: device.value })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}

/// Le changement de langue recharge les catalogues au lieu de recharger la
/// page entiere comme le faisait l'ancienne IHM.
async function changerLangue() {
  const err = await api.put('/api/locale', { locale: lang.value })
  if (err) {
    toast.error(err)
    return
  }
  await chargerTout()
}
</script>

<template>
  <div class="space-y-4">
    <Card>
      <CardHeader><CardTitle>{{ t('status_title') }}</CardTitle></CardHeader>
      <CardContent>
        <table class="w-full text-sm">
          <thead class="text-muted-foreground">
            <tr>
              <th class="text-left font-normal">{{ t('col_plugin') }}</th>
              <th class="text-left font-normal">{{ t('col_kind') }}</th>
              <th class="text-left font-normal">{{ t('col_state') }}</th>
              <th class="text-left font-normal">{{ t('col_admin') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in status.plugins" :key="p.name" class="border-t border-border">
              <td class="py-1">{{ p.name }}</td>
              <td>{{ p.kind }}</td>
              <td>
                <Badge :variant="p.connected ? 'secondary' : 'destructive'">
                  {{ p.connected ? t('connected') : t('unavailable') }}
                </Badge>
              </td>
              <td>
                <RouterLink v-if="p.admin" :to="`/plugins/${p.name}/`" class="underline">
                  {{ t('admin_link') }}
                </RouterLink>
                <span v-else>-</span>
              </td>
            </tr>
          </tbody>
        </table>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('audio_output') }}</CardTitle></CardHeader>
      <CardContent class="flex flex-wrap items-center gap-2">
        <Select v-model="device">
          <SelectTrigger class="min-w-64"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem v-for="d in audio.devices" :key="d" :value="d">{{ d }}</SelectItem>
          </SelectContent>
        </Select>
        <Button @click="changerSortie">{{ t('change') }}</Button>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('language') }}</CardTitle></CardHeader>
      <CardContent class="flex flex-wrap items-center gap-2">
        <Select v-model="lang">
          <SelectTrigger class="min-w-32"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem v-for="l in locale.locales" :key="l" :value="l">{{ l }}</SelectItem>
          </SelectContent>
        </Select>
        <Button @click="changerLangue">{{ t('change') }}</Button>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('recent_errors') }}</CardTitle></CardHeader>
      <CardContent>
        <ul class="space-y-1 font-mono text-xs text-muted-foreground">
          <li v-for="(l, i) in logs" :key="i">{{ l }}</li>
        </ul>
      </CardContent>
    </Card>
  </div>
</template>
```

Dans `App.vue`, remplacer le libellé `status` du lien de navigation par
`{{ t('status_title') }}` en utilisant `useCatalog()` (et appeler `reload()`
dans son `onMounted`).

- [ ] **Step 6: Lancer les tests — succès attendu**

Run : `cargo test -p ritornello-core && cargo clippy -p ritornello-core --all-targets -- -D warnings && npm test -w app`
Expected : SUCCÈS. `status.rs` doit avoir nettement maigri (plus de
`status_page`, plus d'`escape_html`).

- [ ] **Step 7: Commit**

```bash
git add crates/ritornello-core/src/status.rs web/app/src
git commit -m "feat(web): vues accueil et statut en Vue, route /api/logs, retrait du HTML genere par le coeur"
```

---

### Task 10: Protocole admin — `GetAsset` et `GetCatalog` remplacent `GetPage`

Le protocole cesse de transporter une page HTML pour transporter des **actifs
opaques** et des **catalogues**. Task transverse : `proto`, SDK, cœur, et les
deux plugins livrés — c'est la seule façon de garder le workspace compilable.

**État intermédiaire assumé :** à la fin de cette task, les pages d'admin
affichent « IHM indisponible » dans le navigateur, car aucun module `ui.js`
réel n'existe encore (un bouchon tient la place). Les Tasks 11 et 12 les
rétablissent. Rien n'est déployé entre-temps.

**Files:**
- Modify: `crates/ritornello-proto/src/admin.rs`
- Modify: `crates/ritornello-plugin-sdk/src/server.rs`, `src/client.rs`
- Modify: `crates/ritornello-core/src/admin.rs`, `src/status.rs`
- Create: `crates/ritornello-plugin-radio/build.rs`, `crates/ritornello-plugin-generic-input/build.rs`
- Create: `crates/ritornello-plugin-radio/src/placeholder.rs`, `crates/ritornello-plugin-generic-input/src/placeholder.rs`
- Modify: `crates/ritornello-plugin-radio/src/admin.rs`, `crates/ritornello-plugin-generic-input/src/admin.rs`
- Delete: `crates/ritornello-plugin-radio/src/index.html`, `crates/ritornello-plugin-generic-input/src/index.html`

**Interfaces:**
- Consumes: `Catalog::entries()` (Task 7).
- Produces:
  - `AdminReq::GetAsset(String)`, `AdminReq::GetCatalog` (à la place de `GetPage`)
  - `AdminResult::Asset { mime: String, body: Option<String> }`, `AdminResult::Catalog(serde_json::Value)` (à la place de `Page(String)`)
  - `AdminPlugin::asset(&self, path: &str) -> Option<(String, String)>` et `AdminPlugin::catalog(&self) -> serde_json::Value` (à la place de `page`)
  - `AdminClient::get_asset(&self, path: &str) -> Result<Option<(String, String)>>` et `AdminClient::get_catalog(&self) -> Result<serde_json::Value>`
  - `AdminBackend::asset` / `AdminBackend::catalog` (à la place de `page`)
  - `pub fn ui_placeholder_js(commande: &str) -> String` dans chaque plugin
  - Routes `GET /plugins/:name/ui.js`, `/ui.css`, `/api/i18n`

- [ ] **Step 1: Écrire les tests du protocole (doivent échouer)**

Dans `crates/ritornello-proto/src/admin.rs`, `mod tests` — **remplacer**
`request_getpage_roundtrip` par :

```rust
    #[test]
    fn request_getasset_roundtrip() {
        let r = AdminRequest { id: 1, req: AdminReq::GetAsset("ui.js".into()) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":1,"req":"GetAsset","arg":"ui.js"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetAsset("ui.js".into()));
    }

    #[test]
    fn request_getcatalog_roundtrip() {
        let r = AdminRequest { id: 2, req: AdminReq::GetCatalog };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":2,"req":"GetCatalog"}"#);
        let back: AdminRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req, AdminReq::GetCatalog);
    }

    #[test]
    fn resultat_asset_roundtrip_present_et_absent() {
        for r in [
            AdminResult::Asset { mime: "text/javascript".into(), body: Some("export default 1".into()) },
            // `None` est la reponse normale a un chemin inconnu : le coeur la
            // traduit en 404 sans avoir a interpreter le chemin.
            AdminResult::Asset { mime: "text/plain".into(), body: None },
        ] {
            let json = serde_json::to_string(&AdminResponse { id: 3, result: r.clone() }).unwrap();
            let back: AdminResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(back.result, r);
        }
    }

    #[test]
    fn resultat_catalog_roundtrip() {
        let r = AdminResult::Catalog(serde_json::json!({ "btn_save": "Enregistrer" }));
        let json = serde_json::to_string(&AdminResponse { id: 4, result: r.clone() }).unwrap();
        let back: AdminResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.result, r);
    }
```

Dans `crates/ritornello-plugin-sdk/src/server.rs`, `mod tests` : dans le test
de dialogue bout en bout, remplacer la trame `GetPage` par les deux nouvelles
et vérifier le chemin inconnu :

```rust
        write.write_all(b"{\"id\":1,\"req\":\"GetAsset\",\"arg\":\"ui.js\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Asset { body: Some(ref b), .. } if b.contains("contract")));

        write.write_all(b"{\"id\":4,\"req\":\"GetAsset\",\"arg\":\"inconnu.txt\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Asset { body: None, .. }));

        write.write_all(b"{\"id\":5,\"req\":\"GetCatalog\"}\n").await.unwrap();
        let l = lines.next_line().await.unwrap().unwrap();
        let r: AdminResponse = serde_json::from_str(&l).unwrap();
        assert!(matches!(r.result, AdminResult::Catalog(ref v) if v["btn_save"] == "Enregistrer"));
```

et adapter `FakeAdmin` en conséquence :

```rust
        fn asset(&self, path: &str) -> Option<(String, String)> {
            match path {
                "ui.js" => Some(("text/javascript".into(), "export const contract = 1".into())),
                _ => None,
            }
        }
        fn catalog(&self) -> serde_json::Value {
            serde_json::json!({ "btn_save": "Enregistrer" })
        }
```

Dans `crates/ritornello-core/src/admin.rs`, `mod tests` — remplacer
`get_page_sert_le_html` par :

```rust
    #[tokio::test]
    async fn ui_js_est_servi_avec_son_type_et_un_etag() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "text/javascript");
        assert!(resp.headers().contains_key("etag"));
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("contract"));
    }

    #[tokio::test]
    async fn ui_js_est_mis_en_cache_apres_le_premier_acces() {
        // Un bundle est immuable pour la duree de vie du processus du plugin :
        // le relire par IPC a chaque rechargement de page serait du gaspillage.
        let fake = Fake::default();
        let appels = fake.appels_asset.clone();
        let state = state_with(fake);
        let app = router(state);
        for _ in 0..3 {
            let resp = app
                .clone()
                .oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        assert_eq!(appels.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn if_none_match_repond_304() {
        let app = router(state_with(Fake::default()));
        let premier = app
            .clone()
            .oneshot(Request::get("/plugins/radio/ui.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let etag = premier.headers()["etag"].to_str().unwrap().to_string();
        let second = app
            .oneshot(
                Request::get("/plugins/radio/ui.js")
                    .header("if-none-match", etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn un_actif_inconnu_du_plugin_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/ui.css").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn le_catalogue_du_plugin_est_servi_a_plat() {
        let app = router(state_with(Fake::default()));
        let resp = app
            .oneshot(Request::get("/plugins/radio/api/i18n").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["btn_save"], "Enregistrer");
    }

    #[tokio::test]
    async fn ui_js_dun_plugin_inconnu_repond_404() {
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/inconnu/ui.js").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn la_page_dadmin_reste_servie_par_la_spa() {
        // Point de vigilance : la nouvelle route `/plugins/:name/:fichier` ne
        // doit pas capter `/plugins/<nom>/` (segment final vide), qui doit
        // continuer de tomber sur le repli et servir le shell — c'est l'URL
        // historique, presente dans le README et dans les liens de la page de
        // statut.
        let app = router(state_with(Fake::default()));
        let resp = app.oneshot(Request::get("/plugins/radio/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let html = String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(html.contains("__RITORNELLO_THEME__"));
    }
```

Le faux devient compteur, pour prouver la mise en cache :

```rust
    #[derive(Default)]
    struct Fake {
        reject: bool,
        down: bool,
        appels_asset: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AdminBackend for Fake {
        async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
            if self.down { anyhow::bail!("down") }
            self.appels_asset.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(match path {
                "ui.js" => Some(("text/javascript".to_string(), "export const contract = 1".to_string())),
                _ => None,
            })
        }
        async fn catalog(&self) -> Result<serde_json::Value> {
            if self.down { anyhow::bail!("down") }
            Ok(serde_json::json!({ "btn_save": "Enregistrer" }))
        }
        // get_data / set_data : inchangés
    }
```

Adapter les constructions existantes de `Fake` (`Fake { reject: false, down: false }`
→ `Fake::default()`, `Fake { reject: true, ..Default::default() }`, etc.).

- [ ] **Step 2: Lancer les tests — échec attendu**

Run : `cargo test --workspace`
Expected : ÉCHEC de compilation en cascade — `GetPage` / `Page` inexistants,
`asset` / `catalog` absents des traits.

- [ ] **Step 3: Modifier le protocole**

`crates/ritornello-proto/src/admin.rs` :

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", content = "arg")]
pub enum AdminReq {
    /// Actif d'IHM du plugin (`"ui.js"`, `"ui.css"`). Le chemin est **opaque**
    /// pour le cœur : c'est le plugin qui décide ce qu'il expose.
    GetAsset(String),
    /// Catalogue i18n du plugin dans la langue courante, à plat.
    GetCatalog,
    GetData,
    SetData(serde_json::Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AdminResult {
    /// `body: None` = chemin inconnu du plugin (le cœur répond 404). Le `mime`
    /// est fourni par le plugin : le cœur ne déduit rien d'une extension.
    Asset { mime: String, body: Option<String> },
    Catalog(serde_json::Value),
    Data(serde_json::Value),
    Set { ok: bool, error: Option<String> },
}
```

- [ ] **Step 4: Modifier le SDK**

`crates/ritornello-plugin-sdk/src/server.rs` — le trait :

```rust
#[async_trait]
pub trait AdminPlugin: Send + 'static {
    /// Actif d'IHM : `(mime, corps)`, ou `None` si le chemin est inconnu.
    /// Typiquement `ui.js` et `ui.css`, embarqués par `include_str!`.
    fn asset(&self, path: &str) -> Option<(String, String)>;
    /// Catalogue i18n du plugin dans la langue courante, à plat.
    fn catalog(&self) -> serde_json::Value;
    async fn get_data(&self) -> serde_json::Value;
    async fn set_data(&mut self, data: serde_json::Value) -> Result<(), String>;
}
```

et la boucle de service :

```rust
            AdminReq::GetAsset(path) => match plugin.asset(&path) {
                Some((mime, body)) => AdminResult::Asset { mime, body: Some(body) },
                None => AdminResult::Asset { mime: "text/plain".to_string(), body: None },
            },
            AdminReq::GetCatalog => AdminResult::Catalog(plugin.catalog()),
```

`crates/ritornello-plugin-sdk/src/client.rs` — remplacer `get_page` par :

```rust
    pub async fn get_asset(&self, path: &str) -> Result<Option<(String, String)>> {
        match self.request(AdminReq::GetAsset(path.to_string())).await? {
            AdminResult::Asset { mime, body } => Ok(body.map(|b| (mime, b))),
            autre => anyhow::bail!("reponse inattendue a GetAsset: {autre:?}"),
        }
    }

    pub async fn get_catalog(&self) -> Result<serde_json::Value> {
        match self.request(AdminReq::GetCatalog).await? {
            AdminResult::Catalog(v) => Ok(v),
            autre => anyhow::bail!("reponse inattendue a GetCatalog: {autre:?}"),
        }
    }
```

(conserver la forme exacte de l'ancien `get_page`, y compris son passage par
`self.request` et son `timeout` de 5 s.)

- [ ] **Step 5: Modifier le cœur**

`crates/ritornello-core/src/admin.rs` — le trait et son implémentation :

```rust
#[async_trait::async_trait]
pub trait AdminBackend: Send + Sync {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>>;
    async fn catalog(&self) -> Result<serde_json::Value>;
    async fn get_data(&self) -> Result<serde_json::Value>;
    async fn set_data(&self, data: serde_json::Value) -> Result<Result<(), String>>;
}

#[async_trait::async_trait]
impl AdminBackend for ritornello_plugin_sdk::AdminClient {
    async fn asset(&self, path: &str) -> Result<Option<(String, String)>> {
        self.get_asset(path).await
    }
    async fn catalog(&self) -> Result<serde_json::Value> {
        ritornello_plugin_sdk::AdminClient::get_catalog(self).await
    }
    // get_data / set_data : inchangés
}
```

Le cache et les routes, en remplacement de `admin_page` :

```rust
/// Actifs d'IHM déjà récupérés, par `(plugin, chemin)` → `(mime, corps, etag)`.
/// Un bundle est immuable pour la durée de vie du processus du plugin : on ne
/// le relit pas par IPC à chaque rechargement de page.
pub type AssetCache = tokio::sync::RwLock<
    std::collections::HashMap<(String, String), (String, String, String)>,
>;

fn etag_of(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut h);
    format!("\"{:x}\"", h.finish())
}

/// `ui.js` ou `ui.css` d'un plugin. Le nom du fichier vient du chemin de la
/// route, jamais d'une liste en dur : le cœur ne sait pas ce qu'un plugin
/// expose.
pub async fn admin_asset(
    State(st): State<AppState>,
    Path((name, fichier)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(backend) = st.admin_backends.get(&name) else {
        return (StatusCode::NOT_FOUND, "plugin inconnu").into_response();
    };
    let cle = (name.clone(), fichier.clone());
    let en_cache = st.admin_assets.read().await.get(&cle).cloned();
    let (mime, body, etag) = match en_cache {
        Some(v) => v,
        None => match backend.asset(&fichier).await {
            Ok(Some((mime, body))) => {
                let etag = etag_of(&body);
                let v = (mime, body, etag);
                st.admin_assets.write().await.insert(cle, v.clone());
                v
            }
            Ok(None) => return (StatusCode::NOT_FOUND, "actif inconnu").into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (asset {fichier}): {e}");
                return (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response();
            }
        },
    };
    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [
            (axum::http::header::CONTENT_TYPE, mime.as_str()),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
            (axum::http::header::ETAG, etag.as_str()),
        ],
        body,
    )
        .into_response()
}

pub async fn admin_i18n(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.admin_backends.get(&name) {
        None => (StatusCode::NOT_FOUND, "plugin inconnu").into_response(),
        Some(backend) => match backend.catalog().await {
            Ok(v) => Json(v).into_response(),
            Err(e) => {
                tracing::warn!("plugin {name} admin injoignable (catalog): {e}");
                (StatusCode::BAD_GATEWAY, "plugin injoignable").into_response()
            }
        },
    }
}
```

Dans `status.rs` : ajouter `pub admin_assets: Arc<crate::admin::AssetCache>` à
`AppState` (initialisé à `Arc::new(Default::default())` dans `main.rs` et dans
les constructeurs de test), **retirer** la route `/plugins/:name/` et ajouter :

```rust
        .route("/plugins/:name/:fichier", get(crate::admin::admin_asset))
        .route("/plugins/:name/api/i18n", get(crate::admin::admin_i18n))
```

L'ordre compte : `/plugins/:name/api/data` et `/plugins/:name/api/i18n`
doivent être déclarées **avant** `/plugins/:name/:fichier`, sinon un segment
`api` serait pris pour un nom de fichier.

- [ ] **Step 6: Adapter les deux plugins (bouchon + nouveau trait)**

Créer, **à l'identique** dans les deux crates de plugin,
`src/placeholder.rs` :

```rust
//! Module ESM servi tant que l'IHM du plugin n'a pas été construite.
//!
//! Inclus **textuellement** par `build.rs` (`include!`) autant que compilé
//! comme module du crate : c'est ce qui permet de tester la fabrication du
//! bouchon par `cargo test`, alors que Cargo n'exécute jamais les tests d'un
//! script de build. Aucune dépendance externe autorisée ici.

/// Contrat volontairement invalide : le shell affiche alors son message
/// « plugin à reconstruire », qui décrit exactement la situation.
pub fn ui_placeholder_js(commande: &str) -> String {
    format!("// IHM non construite. Lancer : {commande}\nexport const contract = -1;\n")
}
```

et `build.rs` :

```rust
// Garantit l'existence de `ui/dist/{ui.js,ui.css}` embarqués par
// `include_str!`. Le build npm n'est **jamais** invoqué ici (voir
// `deploy/build.sh`) : la cross-compilation tourne dans une image sans Node.
include!("src/placeholder.rs");

fn main() {
    println!("cargo::rerun-if-changed=ui/dist");
    println!("cargo::rerun-if-changed=src/placeholder.rs");
    let dist = std::path::Path::new("ui/dist");
    std::fs::create_dir_all(dist).expect("creation de ui/dist");
    let js = dist.join("ui.js");
    if !js.exists() {
        println!("cargo::warning=IHM du plugin non construite : bouchon embarque");
        std::fs::write(&js, ui_placeholder_js("npm ci && npm run build --workspaces")).unwrap();
    }
    let css = dist.join("ui.css");
    if !css.exists() {
        std::fs::write(&css, "/* IHM non construite */\n").unwrap();
    }
}
```

Dans `src/admin.rs` de **chaque** plugin : déclarer `mod placeholder;` dans le
`main.rs` du crate, **supprimer** `fn page()`, la boucle de substitution des
jetons, la constante `PAGE_KEYS`, et les trois tests devenus sans objet
(`page_substitue_les_jetons_avec_le_catalogue`,
`page_ne_laisse_aucun_jeton_non_substitue`,
`toutes_les_cles_de_page_existent_dans_len_embarque`) ainsi que le garde-fou
`aucune_valeur_ne_contient_un_caractere_dangereux_pour_la_substitution` —
il n'y a plus de substitution, donc plus de caractère dangereux (c'est le
follow-up « échappement structurel » de la revue, refermé ici). **Conserver**
`parite_des_cles_entre_len_embarque_et_le_pack_fr`, qui ne dépend pas du
mécanisme de rendu.

Mettre à la place :

```rust
    fn asset(&self, path: &str) -> Option<(String, String)> {
        match path {
            "ui.js" => Some((
                "text/javascript".to_string(),
                include_str!("../ui/dist/ui.js").to_string(),
            )),
            "ui.css" => Some((
                "text/css".to_string(),
                include_str!("../ui/dist/ui.css").to_string(),
            )),
            _ => None,
        }
    }

    fn catalog(&self) -> serde_json::Value {
        let cat = self.catalog.read().unwrap();
        serde_json::json!(cat.entries())
    }
```

(dans le plugin radio, l'accès au catalogue suit la forme déjà utilisée par
l'ancien `page()` ; ne pas tenir de garde `std::sync::RwLock` au travers d'un
`.await` — `catalog()` est synchrone, donc le risque n'existe pas.)

Ajouter dans chaque `mod tests` :

```rust
    #[test]
    fn asset_expose_ui_js_et_ui_css_et_rien_dautre() {
        let dir = tempfile::tempdir().unwrap();
        let a = admin(dir.path());
        let (mime, corps) = a.asset("ui.js").unwrap();
        assert_eq!(mime, "text/javascript");
        assert!(!corps.is_empty());
        assert_eq!(a.asset("ui.css").unwrap().0, "text/css");
        // Un chemin inconnu n'est pas une erreur : c'est un 404 cote coeur.
        assert!(a.asset("../../../etc/passwd").is_none());
        assert!(a.asset("index.html").is_none());
    }

    #[test]
    fn catalog_expose_les_cles_du_composant() {
        let dir = tempfile::tempdir().unwrap();
        let v = admin(dir.path()).catalog();
        assert!(v["btn_save"].is_string(), "le catalogue doit porter les cles du plugin");
    }
```

Enfin, **supprimer** `crates/ritornello-plugin-radio/src/index.html` et
`crates/ritornello-plugin-generic-input/src/index.html`, et ajouter
`crates/*/ui/dist/` au `.gitignore` (déjà couvert par la règle `dist/`).

- [ ] **Step 7: Lancer les tests — succès attendu**

Run : `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected : SUCCÈS.

- [ ] **Step 8: Commit**

```bash
git add crates .gitignore
git rm crates/ritornello-plugin-radio/src/index.html crates/ritornello-plugin-generic-input/src/index.html
git commit -m "feat(proto): le protocole admin transporte des actifs et des catalogues (GetAsset/GetCatalog)"
```

---

### Task 11: Module IHM du plugin radio

Première IHM de plugin réelle : stations, numérotation automatique, limite de
9, recherche annuaire avec sa garde de vol unique.

**Files:**
- Create: `crates/ritornello-plugin-radio/ui/package.json`, `tsconfig.json`, `vite.config.ts`, `vitest.config.ts`, `src/index.ts`, `src/RadioAdmin.vue`, `src/ui.css`, `src/RadioAdmin.test.ts`
- Modify: `package-lock.json`

**Interfaces:**
- Consumes: `@ritornello/ui` (kit complet), `vue` — tous deux **externes** ;
  `GET/PUT /plugins/radio/api/data` avec les opérations `save` et `search`
  (inchangées, cf. spec annuaire) ; le catalogue via la prop injectée par
  `PluginRoute.vue`.
- Produces: `export const contract = 1` et le composant par défaut dans
  `crates/ritornello-plugin-radio/ui/dist/ui.js`, plus `ui.css`.

- [ ] **Step 1: Créer le paquet du module**

```bash
npm i -w ritornello-plugin-radio-ui -D vite @vitejs/plugin-vue vue @ritornello/ui tailwindcss @tailwindcss/vite typescript vitest jsdom @vue/test-utils
```

`crates/ritornello-plugin-radio/ui/package.json` :

```json
{
  "name": "ritornello-plugin-radio-ui",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": { "build": "vite build", "test": "vitest run" }
}
```

`crates/ritornello-plugin-radio/ui/vite.config.ts` :

```ts
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

// `vue` et `@ritornello/ui` sont **externes** : ils sont fournis par l'import
// map du shell. Le module ne pese donc que sa propre logique, et partage
// l'unique instance de Vue de la page.
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  build: {
    lib: { entry: 'src/index.ts', formats: ['es'], fileName: () => 'ui.js' },
    rollupOptions: {
      external: ['vue', '@ritornello/ui'],
      output: { assetFileNames: 'ui.css' },
    },
    cssCodeSplit: false,
    emptyOutDir: true,
  },
})
```

`crates/ritornello-plugin-radio/ui/src/ui.css` — **sa propre** passe Tailwind,
sans préflight (le shell l'a déjà appliqué : le réinitialiser deux fois
casserait la mise en page) :

```css
@import "tailwindcss/theme.css" layer(theme);
@import "tailwindcss/utilities.css" layer(utilities);
@import "@ritornello/ui/theme.css";

/* Tailwind ne genere que les classes qu'il voit : ce `@source` couvre les
   classes propres a ce module. Celles des composants du kit viennent du CSS
   du shell. */
@source "./";
```

`crates/ritornello-plugin-radio/ui/vitest.config.ts` — le plugin Vue est
nécessaire pour monter le SFC en test :

```ts
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [vue()],
  test: { environment: 'jsdom', globals: true },
})
```

`crates/ritornello-plugin-radio/ui/tsconfig.json` :

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "lib": ["ES2022", "DOM"],
    "types": ["vitest/globals"],
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["src", "*.ts"]
}
```

- [ ] **Step 2: Écrire les tests (doivent échouer)**

`crates/ritornello-plugin-radio/ui/src/RadioAdmin.test.ts` :

```ts
import { flushPromises, mount } from '@vue/test-utils'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import RadioAdmin from './RadioAdmin.vue'

const CATALOGUE = {
  btn_add: 'Ajouter', btn_save: 'Enregistrer', btn_search: 'Chercher',
  btn_add_result: '+', saved: 'Enregistré', save_error: 'Échec : ',
  limit_reached: '9 maximum', empty_query: 'Saisir un terme',
  searching: 'Recherche…', no_results: 'Aucun résultat',
  col_num: 'N°', col_name: 'Nom', col_url: 'URL',
  search_title: 'Annuaire', search_placeholder: 'nom', country_label: 'Pays',
  country_fr: 'France', country_us: 'États-Unis', country_all: 'Tous',
  load_error_1: 'Erreur : ', load_error_2: '',
}

function reponses(data: unknown) {
  return vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === 'PUT') return new Response(null, { status: 204 })
    return new Response(JSON.stringify(data), { status: 200 })
  })
}

async function monter(data: unknown = { stations: [], search: [] }) {
  const spy = reponses(data)
  vi.stubGlobal('fetch', spy)
  const w = mount(RadioAdmin, { props: { catalog: CATALOGUE } })
  await flushPromises()
  return { w, spy }
}

describe('RadioAdmin', () => {
  beforeEach(() => vi.unstubAllGlobals())

  it('charge les stations triées par présélection', async () => {
    const { w } = await monter({
      stations: [
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 1, name: 'A', url: 'http://a' },
      ],
      search: [],
    })
    const noms = w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value)
    expect(noms).toEqual(['A', 'B'])
  })

  it('numérote par position et renumérote après suppression', async () => {
    const { w } = await monter({
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: 'B', url: 'http://b' },
        { preset: 3, name: 'C', url: 'http://c' },
      ],
      search: [],
    })
    await w.findAll('[data-station-delete]')[0]!.trigger('click')
    expect(w.findAll('[data-station-num]').map((n) => n.text())).toEqual(['1', '2'])
    expect(w.findAll('[data-station-name]').map((i) => (i.element as HTMLInputElement).value))
      .toEqual(['B', 'C'])
  })

  it('refuse une dixième station avec un message', async () => {
    const stations = Array.from({ length: 9 }, (_, i) => ({
      preset: i + 1, name: `S${i}`, url: `http://s${i}`,
    }))
    const { w } = await monter({ stations, search: [] })
    await w.find('[data-add]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(9)
    expect(w.text()).toContain('9 maximum')
  })

  it('envoie la présélection déduite de la position à l’enregistrement', async () => {
    const { w, spy } = await monter({
      stations: [{ preset: 1, name: 'A', url: 'http://a' }],
      search: [],
    })
    await w.find('[data-add]').trigger('click')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const appel = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((appel![1] as RequestInit).body))).toEqual({
      op: 'save',
      stations: [
        { preset: 1, name: 'A', url: 'http://a' },
        { preset: 2, name: '', url: '' },
      ],
    })
  })

  it('recherche dans l’annuaire puis relit les résultats', async () => {
    const { w, spy } = await monter({
      stations: [],
      search: [{ name: 'FIP', url: 'http://fip', codec: 'MP3', bitrate: 128, country: 'FR' }],
    })
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')
    expect(JSON.parse(String((put![1] as RequestInit).body))).toEqual({
      op: 'search', query: 'fip', country: 'FR',
    })
    expect(w.text()).toContain('FIP')
    expect(w.text()).toContain('128')
  })

  it('une requête vide n’émet rien et affiche le message dédié', async () => {
    const { w, spy } = await monter()
    await w.find('[data-query]').setValue('   ')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
    expect(w.text()).toContain('Saisir un terme')
  })

  it('vol unique : un second déclenchement pendant une recherche n’émet rien', async () => {
    // Le SDK sert les requetes d'admin strictement en serie : une seconde
    // recherche mise en file derriere la premiere depasserait le plafond de
    // 5 s du coeur, qui afficherait « plugin injoignable ».
    let debloquer: () => void = () => {}
    const enCours = new Promise<void>((r) => (debloquer = r))
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') {
        await enCours
        return new Response(null, { status: 204 })
      }
      return new Response(JSON.stringify({ stations: [], search: [] }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE } })
    await flushPromises()
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await w.find('[data-search]').trigger('click')
    await w.find('[data-query]').trigger('keydown', { key: 'Enter' })
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(1)
    debloquer()
    await flushPromises()
    // L'etat est rétabli : une nouvelle recherche redevient possible.
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(2)
  })

  it('vol unique : l’état est rétabli même après une erreur', async () => {
    const spy = vi.fn(async (_url: string, init?: RequestInit) => {
      if (init?.method === 'PUT') return new Response(JSON.stringify({ error: 'annuaire muet' }), { status: 422 })
      return new Response(JSON.stringify({ stations: [], search: [] }), { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const w = mount(RadioAdmin, { props: { catalog: CATALOGUE } })
    await flushPromises()
    await w.find('[data-query]').setValue('fip')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(w.text()).toContain('annuaire muet')
    await w.find('[data-search]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.filter((c) => (c[1] as RequestInit)?.method === 'PUT')).toHaveLength(2)
  })

  it('ajoute un résultat de recherche dans la table en cours d’édition', async () => {
    const { w } = await monter({
      stations: [],
      search: [{ name: 'FIP', url: 'http://fip', codec: 'MP3', bitrate: 128, country: 'FR' }],
    })
    await w.find('[data-add-result]').trigger('click')
    expect(w.findAll('[data-station-num]')).toHaveLength(1)
    expect((w.find('[data-station-name]').element as HTMLInputElement).value).toBe('FIP')
  })
})
```

- [ ] **Step 3: Lancer les tests — échec attendu**

Run : `npm test -w ritornello-plugin-radio-ui`
Expected : ÉCHEC — `./RadioAdmin.vue` introuvable.

- [ ] **Step 4: Écrire le composant**

`crates/ritornello-plugin-radio/ui/src/RadioAdmin.vue` :

```vue
<script setup lang="ts">
import {
  api, Button, createT, Input, type Catalog,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

const props = defineProps<{ catalog: Catalog }>()
const t = computed(() => createT(props.catalog))

/// Les chiffres de la telecommande : au-dela, l'ajout est refuse cote IHM
/// plutot que de laisser l'enregistrement echouer. `Stations::validate` reste
/// l'autorite serveur.
const MAX = 9

interface Station { name: string; url: string }
interface Trouvee { name: string; url: string; codec: string; bitrate: number; country: string }

const stations = ref<Station[]>([])
const resultats = ref<Trouvee[] | null>(null)
const query = ref('')
const pays = ref('FR')
const message = ref('')
const rechercheEnCours = ref(false)

async function recharger(): Promise<void> {
  try {
    const data = await api.get<{ stations: Array<Station & { preset: number }>; search?: Trouvee[] }>(
      './api/data',
    )
    stations.value = [...data.stations]
      .sort((a, b) => a.preset - b.preset)
      .map(({ name, url }) => ({ name, url }))
    if (data.search?.length) resultats.value = data.search
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
  }
}

onMounted(recharger)

/// Rien n'est persiste avant « Enregistrer » : l'ajout n'agit que sur la table
/// en cours d'edition.
function ajouter(s: Station = { name: '', url: '' }): boolean {
  if (stations.value.length >= MAX) {
    message.value = t.value('limit_reached')
    return false
  }
  stations.value.push({ ...s })
  message.value = ''
  return true
}

function supprimer(i: number): void {
  stations.value.splice(i, 1)
}

/// Numerotation automatique : la presélection est la **position** de la ligne.
/// Consequence assumee : supprimer une ligne renumerote les suivantes.
async function enregistrer(): Promise<void> {
  const charge = stations.value.map((s, i) => ({ preset: i + 1, name: s.name, url: s.url }))
  const err = await api.put('./api/data', { op: 'save', stations: charge })
  message.value = err ? t.value('save_error') + err : t.value('saved')
}

/// Vol unique : le SDK sert les requetes d'admin strictement en serie. Un
/// second declenchement pendant qu'une recherche court se mettrait en file
/// derriere la premiere et, l'annuaire etant en panne (budget de 4 s cote
/// plugin), depasserait le plafond de 5 s du coeur — qui afficherait
/// « plugin injoignable ». La garde est partagee par le bouton et la touche
/// Entree, et levee dans un `finally` pour se rétablir aussi bien apres une
/// erreur qu'apres un succes.
async function chercher(): Promise<void> {
  if (rechercheEnCours.value) return
  const q = query.value.trim()
  if (!q) {
    message.value = t.value('empty_query')
    return
  }
  rechercheEnCours.value = true
  message.value = t.value('searching')
  try {
    const err = await api.put('./api/data', { op: 'search', query: q, country: pays.value })
    if (err) {
      message.value = err
      return
    }
    const data = await api.get<{ search?: Trouvee[] }>('./api/data')
    resultats.value = data.search ?? []
    message.value = ''
  } catch (e) {
    message.value = t.value('load_error_1') + (e as Error).message + t.value('load_error_2')
  } finally {
    rechercheEnCours.value = false
  }
}

function libelle(s: Trouvee): string {
  return `${s.name} — ${s.codec} ${s.bitrate} kbps${s.country ? ` (${s.country})` : ''}`
}
</script>

<template>
  <div class="space-y-6">
    <table class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="w-10 text-left font-normal">{{ t('col_num') }}</th>
          <th class="text-left font-normal">{{ t('col_name') }}</th>
          <th class="text-left font-normal">{{ t('col_url') }}</th>
          <th class="w-10" />
        </tr>
      </thead>
      <tbody>
        <tr v-for="(s, i) in stations" :key="i" class="border-t border-border">
          <td data-station-num class="tabular-nums text-muted-foreground">{{ i + 1 }}</td>
          <td class="py-1 pr-2"><Input v-model="s.name" data-station-name /></td>
          <td class="py-1 pr-2"><Input v-model="s.url" data-station-url /></td>
          <td>
            <Button variant="ghost" size="icon" data-station-delete @click="supprimer(i)">✕</Button>
          </td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <Button variant="secondary" data-add @click="ajouter()">{{ t('btn_add') }}</Button>
      <Button data-save @click="enregistrer">{{ t('btn_save') }}</Button>
      <span class="text-sm text-muted-foreground">{{ message }}</span>
    </div>

    <section class="space-y-2">
      <h2 class="font-medium">{{ t('search_title') }}</h2>
      <div class="flex flex-wrap items-center gap-2">
        <Input
          v-model="query"
          data-query
          class="min-w-48 flex-1"
          :placeholder="t('search_placeholder')"
          @keydown.enter="chercher"
        />
        <Select v-model="pays">
          <SelectTrigger class="w-40"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="FR">{{ t('country_fr') }}</SelectItem>
            <SelectItem value="US">{{ t('country_us') }}</SelectItem>
            <SelectItem value="">{{ t('country_all') }}</SelectItem>
          </SelectContent>
        </Select>
        <Button data-search :disabled="rechercheEnCours" @click="chercher">
          {{ t('btn_search') }}
        </Button>
      </div>
      <ul v-if="resultats" class="space-y-1 text-sm">
        <li v-if="!resultats.length" class="text-muted-foreground">{{ t('no_results') }}</li>
        <li v-for="(s, i) in resultats" :key="i" class="flex items-center gap-2">
          <!-- textContent par interpolation, jamais de v-html : le nom vient
               d'un annuaire public. -->
          <span class="flex-1">{{ libelle(s) }}</span>
          <Button
            variant="secondary"
            size="sm"
            data-add-result
            @click="ajouter({ name: s.name, url: s.url })"
          >
            {{ t('btn_add_result') }}
          </Button>
        </li>
      </ul>
    </section>
  </div>
</template>
```

`crates/ritornello-plugin-radio/ui/src/index.ts` :

```ts
import { UI_CONTRACT } from '@ritornello/ui'
import RadioAdmin from './RadioAdmin.vue'
import './ui.css'

/// Version du contrat contre laquelle ce module est compilé. Le shell la
/// compare à la sienne avant de monter le composant.
export const contract = UI_CONTRACT
export default RadioAdmin
```

- [ ] **Step 5: Lancer les tests et le build — succès attendu**

Run : `npm test -w ritornello-plugin-radio-ui && npm run build --workspaces && cargo test -p ritornello-plugin-radio`
Expected : SUCCÈS — 10 tests du composant, et `ui/dist/{ui.js,ui.css}`
produits (le bouchon est écrasé). Vérifier que le bundle reste petit et
n'embarque pas Vue :

```bash
node --input-type=module -e "
import { readFileSync, statSync } from 'node:fs'
const p = 'crates/ritornello-plugin-radio/ui/dist/ui.js'
const s = readFileSync(p, 'utf8')
if (!/from ?[\"'](vue|@ritornello\/ui)[\"']/.test(s)) throw new Error('vue/kit devraient rester externes')
console.log('ui.js', (statSync(p).size / 1024).toFixed(1), 'Ko')
"
```

- [ ] **Step 6: Commit**

```bash
git add crates/ritornello-plugin-radio package-lock.json
git commit -m "feat(radio): IHM en module ESM (stations, numerotation automatique, recherche annuaire en vol unique)"
```

---

### Task 12: Module IHM du plugin generic-input

La plus riche des trois vues : 21 actions, apprentissage de touche par
sondage, presets livrés, import et export de fichiers `.toml`.

**Files:**
- Create: `crates/ritornello-plugin-generic-input/ui/package.json`, `tsconfig.json`, `vite.config.ts`, `vitest.config.ts`, `src/index.ts`, `src/InputAdmin.vue`, `src/preset-toml.ts`, `src/preset-toml.test.ts`, `src/InputAdmin.test.ts`, `src/ui.css`
- Modify: `package-lock.json`

**Interfaces:**
- Consumes: `@ritornello/ui`, `vue` (externes) ; `GET/PUT /plugins/generic-input/api/data` avec les opérations `save`, `learn`, `cancel_learn`, `rescan`, `load_preset`, `import_preset` (inchangées).
- Produces:
  - `export const ACTIONS: Array<{ key: string; cmd: Command }>` — les 21 actions, dans l'ordre de l'ancienne page
  - `export function presetToml(bindings: Binding[]): string`
  - `export function sanitiseDeviceName(name: string): string`
  - `export function codesFor(table: BindingTable, device: string, cmd: Command): string`
  - `export function collect(table: BindingTable, device: string, codes: string[]): BindingTable`
  - `contract` + composant par défaut dans `ui/dist/ui.js`, plus `ui.css`

- [ ] **Step 1: Créer le paquet**

```bash
npm i -w ritornello-plugin-generic-input-ui -D vite @vitejs/plugin-vue vue @ritornello/ui tailwindcss @tailwindcss/vite typescript vitest jsdom @vue/test-utils
```

`crates/ritornello-plugin-generic-input/ui/package.json` :

```json
{
  "name": "ritornello-plugin-generic-input-ui",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": { "build": "vite build", "test": "vitest run" }
}
```

`crates/ritornello-plugin-generic-input/ui/vite.config.ts` :

```ts
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

// `vue` et `@ritornello/ui` sont **externes** : ils sont fournis par l'import
// map du shell. Le module ne pese donc que sa propre logique, et partage
// l'unique instance de Vue de la page.
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  build: {
    lib: { entry: 'src/index.ts', formats: ['es'], fileName: () => 'ui.js' },
    rollupOptions: {
      external: ['vue', '@ritornello/ui'],
      output: { assetFileNames: 'ui.css' },
    },
    cssCodeSplit: false,
    emptyOutDir: true,
  },
})
```

`crates/ritornello-plugin-generic-input/ui/src/ui.css` — **sa propre** passe
Tailwind, sans préflight (le shell l'a déjà appliqué) :

```css
@import "tailwindcss/theme.css" layer(theme);
@import "tailwindcss/utilities.css" layer(utilities);
@import "@ritornello/ui/theme.css";

/* Tailwind ne genere que les classes qu'il voit : ce `@source` couvre les
   classes propres a ce module. Celles des composants du kit viennent du CSS
   du shell. */
@source "./";
```

`crates/ritornello-plugin-generic-input/ui/vitest.config.ts` :

```ts
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [vue()],
  test: { environment: 'jsdom', globals: true },
})
```

`crates/ritornello-plugin-generic-input/ui/tsconfig.json` :

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "strict": true,
    "noUncheckedIndexedAccess": true,
    "lib": ["ES2022", "DOM"],
    "types": ["vitest/globals"],
    "skipLibCheck": true,
    "noEmit": true
  },
  "include": ["src", "*.ts"]
}
```


- [ ] **Step 2: Écrire les tests des fonctions pures (doivent échouer)**

`crates/ritornello-plugin-generic-input/ui/src/preset-toml.test.ts` :

```ts
import { describe, expect, it } from 'vitest'
import { ACTIONS, codesFor, collect, presetToml, sanitiseDeviceName } from './preset-toml'

describe('ACTIONS', () => {
  it('couvre les 21 actions du protocole', () => {
    expect(ACTIONS).toHaveLength(21)
    expect(ACTIONS.slice(0, 9).map((a) => a.cmd.arg)).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9])
    expect(ACTIONS.slice(0, 9).every((a) => a.cmd.cmd === 'Select')).toBe(true)
    expect(ACTIONS.slice(9).map((a) => a.cmd.cmd)).toEqual([
      'Next', 'Prev', 'VolumeUp', 'VolumeDown', 'Mute', 'PlayPause',
      'Stop', 'NextTrack', 'PrevTrack', 'Eject', 'SourceCycle', 'Power',
    ])
  })
})

describe('codesFor', () => {
  const table = {
    devices: [
      { name: 'mce', bindings: [{ code: 1, cmd: 'Select', arg: 1 }, { code: 2, cmd: 'Select', arg: 1 }, { code: 9, cmd: 'Mute' }] },
      { name: 'clavier', bindings: [{ code: 5, cmd: 'Mute' }] },
    ],
  }

  it('joint les codes d’une même action, séparés par des virgules', () => {
    expect(codesFor(table, 'mce', { cmd: 'Select', arg: 1 })).toBe('1, 2')
  })

  it('distingue une commande sans argument', () => {
    expect(codesFor(table, 'mce', { cmd: 'Mute' })).toBe('9')
    expect(codesFor(table, 'clavier', { cmd: 'Mute' })).toBe('5')
  })

  it('renvoie une chaîne vide pour un périphérique ou une action absents', () => {
    expect(codesFor(table, 'inconnu', { cmd: 'Mute' })).toBe('')
    expect(codesFor(table, 'clavier', { cmd: 'Power' })).toBe('')
  })
})

describe('collect', () => {
  it('réécrit le périphérique courant et préserve les autres tels quels', () => {
    const table = {
      devices: [
        { name: 'mce', bindings: [{ code: 1, cmd: 'Select', arg: 1 }] },
        { name: 'clavier', bindings: [{ code: 5, cmd: 'Mute' }] },
      ],
    }
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? '7' : ''))
    const out = collect(table, 'mce', codes)
    expect(out.devices.find((d) => d.name === 'clavier')).toEqual(table.devices[1])
    expect(out.devices.find((d) => d.name === 'mce')!.bindings).toEqual([{ code: 7, cmd: 'Mute' }])
  })

  it('accepte plusieurs codes par action et ignore ce qui n’est pas un nombre', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Mute' ? ' 7 , 8 , abc , ' : ''))
    const out = collect({ devices: [] }, 'mce', codes)
    expect(out.devices[0]!.bindings).toEqual([
      { code: 7, cmd: 'Mute' },
      { code: 8, cmd: 'Mute' },
    ])
  })

  it('n’émet `arg` que lorsqu’il existe', () => {
    const codes = ACTIONS.map((a) => (a.cmd.cmd === 'Select' && a.cmd.arg === 3 ? '4' : ''))
    expect(collect({ devices: [] }, 'mce', codes).devices[0]!.bindings).toEqual([
      { code: 4, cmd: 'Select', arg: 3 },
    ])
  })
})

describe('presetToml', () => {
  it('produit le format lu par `presets::parse_preset`', () => {
    // Miroir exact du format cote Rust
    // (crates/ritornello-plugin-generic-input/src/presets.rs) : toute
    // evolution du format Rust doit etre repercutee ici, sous peine de
    // fichiers exportes que le serveur refuserait.
    const out = presetToml([{ code: 4, cmd: 'Select', arg: 3 }, { code: 9, cmd: 'Mute' }])
    expect(out).toBe(
      '[[bindings]]\ncode = 4\ncmd = "Select"\narg = 3\n\n[[bindings]]\ncode = 9\ncmd = "Mute"\n',
    )
  })

  it('produit une chaîne vide sans binding', () => {
    expect(presetToml([])).toBe('')
  })
})

describe('sanitiseDeviceName', () => {
  it('réduit le nom à un identifiant de fichier sûr', () => {
    expect(sanitiseDeviceName('Media Center Ed. 3/4')).toBe('Media_Center_Ed_3_4')
    expect(sanitiseDeviceName('../../etc/passwd')).toBe('_etc_passwd')
  })
})
```

- [ ] **Step 3: Écrire les tests du composant (doivent échouer)**

`crates/ritornello-plugin-generic-input/ui/src/InputAdmin.test.ts` :

```ts
import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import InputAdmin from './InputAdmin.vue'

const CATALOGUE = {
  device_label: 'Périphérique', btn_refresh: 'Rafraîchir', col_action: 'Action', col_code: 'Code',
  btn_learn: 'Apprendre', btn_clear: 'Effacer', btn_save: 'Enregistrer', btn_cancel: 'Annuler',
  btn_load_preset: 'Charger', btn_import: 'Importer', btn_export: 'Exporter',
  preset_label: 'Preset', learning_msg: 'Appuyez sur une touche', learn_timeout: 'Délai dépassé',
  saved: 'Enregistré', save_error: 'Échec : ', load_error: 'Erreur : ', no_device: 'Aucun périphérique',
  act_mute: 'Muet', act_power: 'Veille',
}

const DATA = {
  devices: ['mce', 'clavier'],
  bindings: { devices: [{ name: 'mce', bindings: [{ code: 9, cmd: 'Mute' }] }] },
  presets: ['mce', 'keyboard'],
  learning: null as { captured: number | null } | null,
}

function stub(data: () => unknown) {
  const spy = vi.fn(async (_u: string, init?: RequestInit) =>
    init?.method === 'PUT'
      ? new Response(null, { status: 204 })
      : new Response(JSON.stringify(data()), { status: 200 }),
  )
  vi.stubGlobal('fetch', spy)
  return spy
}

async function monter(data: () => unknown = () => DATA) {
  const spy = stub(data)
  const w = mount(InputAdmin, { props: { catalog: CATALOGUE } })
  await flushPromises()
  return { w, spy }
}

describe('InputAdmin', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(() => vi.useRealTimers())

  it('liste les périphériques, les presets et les 21 actions', async () => {
    const { w } = await monter()
    expect(w.findAll('[data-action-row]')).toHaveLength(21)
    expect(w.find('[data-device-select]').exists()).toBe(true)
  })

  it('préremplit les codes du périphérique sélectionné', async () => {
    const { w } = await monter()
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    expect((muet.find('input').element as HTMLInputElement).value).toBe('9')
  })

  it('efface un code sans toucher au serveur', async () => {
    const { w, spy } = await monter()
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-clear]').trigger('click')
    expect((muet.find('input').element as HTMLInputElement).value).toBe('')
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('enregistre la table complète du périphérique courant', async () => {
    const { w, spy } = await monter()
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    const put = spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')!
    const corps = JSON.parse(String((put[1] as RequestInit).body))
    expect(corps.op).toBe('save')
    expect(corps.bindings.devices.find((d: { name: string }) => d.name === 'mce').bindings).toEqual([
      { code: 9, cmd: 'Mute' },
    ])
  })

  it('apprentissage : sonde toutes les 300 ms puis remplit le code capturé', async () => {
    vi.useFakeTimers()
    let captured: number | null = null
    const spy = stub(() => ({ ...DATA, learning: { captured } }))
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE } })
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(0)
    expect(w.text()).toContain('Appuyez sur une touche')
    expect(
      JSON.parse(String((spy.mock.calls.find((c) => (c[1] as RequestInit)?.method === 'PUT')![1] as RequestInit).body)),
    ).toEqual({ op: 'learn', device: 'mce' })
    captured = 42
    await vi.advanceTimersByTimeAsync(300)
    expect((muet.find('input').element as HTMLInputElement).value).toBe('42')
    // Le sondage s'arrete et `cancel_learn` est emis.
    const ops = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    expect(ops).toContain('cancel_learn')
  })

  it('apprentissage : abandonne après 10 s avec le message de délai', async () => {
    vi.useFakeTimers()
    stub(() => ({ ...DATA, learning: { captured: null } }))
    const w = mount(InputAdmin, { props: { catalog: CATALOGUE } })
    await vi.advanceTimersByTimeAsync(0)
    const muet = w.findAll('[data-action-row]').find((r) => r.text().includes('Muet'))!
    await muet.find('[data-learn]').trigger('click')
    await vi.advanceTimersByTimeAsync(10_500)
    expect(w.text()).toContain('Délai dépassé')
  })

  it('sans périphérique, prévient et n’émet aucune opération', async () => {
    const { w, spy } = await monter(() => ({ ...DATA, devices: [] }))
    expect(w.text()).toContain('Aucun périphérique')
    await w.find('[data-save]').trigger('click')
    await flushPromises()
    expect(spy.mock.calls.some((c) => (c[1] as RequestInit)?.method === 'PUT')).toBe(false)
  })

  it('rafraîchir envoie `rescan` puis recharge', async () => {
    const { w, spy } = await monter()
    await w.find('[data-refresh]').trigger('click')
    await flushPromises()
    const ops = spy.mock.calls
      .filter((c) => (c[1] as RequestInit)?.method === 'PUT')
      .map((c) => JSON.parse(String((c[1] as RequestInit).body)).op)
    expect(ops).toEqual(['rescan'])
  })

  it('importe un fichier `.toml` en le confiant au serveur', async () => {
    const { w, spy } = await monter()
    const fichier = new File(['[[bindings]]\ncode = 1\ncmd = "Mute"\n'], 'p.toml')
    const input = w.find('[data-import]').element as HTMLInputElement
    Object.defineProperty(input, 'files', { value: [fichier] })
    await w.find('[data-import]').trigger('change')
    await vi.waitFor(() =>
      expect(
        spy.mock.calls.some(
          (c) =>
            (c[1] as RequestInit)?.method === 'PUT' &&
            JSON.parse(String((c[1] as RequestInit).body)).op === 'import_preset',
        ),
      ).toBe(true),
    )
  })
})
```

- [ ] **Step 4: Lancer les tests — échec attendu**

Run : `npm test -w ritornello-plugin-generic-input-ui`
Expected : ÉCHEC — `./preset-toml` et `./InputAdmin.vue` introuvables.

- [ ] **Step 5: Écrire les fonctions pures**

`crates/ritornello-plugin-generic-input/ui/src/preset-toml.ts` :

```ts
export interface Command { cmd: string; arg?: number }
export interface Binding extends Command { code: number }
export interface DeviceBindings { name: string; bindings: Binding[] }
export interface BindingTable { devices: DeviceBindings[] }

/// Les 21 actions, dans l'ordre de l'ancienne page. Le libelle est traduit par
/// le catalogue du plugin (cle `key`), la commande est un
/// `ritornello_proto::Command` serialise (`cmd`/`arg`).
export const ACTIONS: Array<{ key: string; cmd: Command }> = [
  ...Array.from({ length: 9 }, (_, i) => ({
    key: `act_select_${i + 1}`,
    cmd: { cmd: 'Select', arg: i + 1 },
  })),
  { key: 'act_next', cmd: { cmd: 'Next' } },
  { key: 'act_prev', cmd: { cmd: 'Prev' } },
  { key: 'act_volume_up', cmd: { cmd: 'VolumeUp' } },
  { key: 'act_volume_down', cmd: { cmd: 'VolumeDown' } },
  { key: 'act_mute', cmd: { cmd: 'Mute' } },
  { key: 'act_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'act_stop', cmd: { cmd: 'Stop' } },
  { key: 'act_next_track', cmd: { cmd: 'NextTrack' } },
  { key: 'act_prev_track', cmd: { cmd: 'PrevTrack' } },
  { key: 'act_eject', cmd: { cmd: 'Eject' } },
  { key: 'act_source_cycle', cmd: { cmd: 'SourceCycle' } },
  { key: 'act_power', cmd: { cmd: 'Power' } },
]

const memeCmd = (a: Command, b: Command) => a.cmd === b.cmd && (a.arg ?? null) === (b.arg ?? null)

export function codesFor(table: BindingTable, device: string, cmd: Command): string {
  const d = table.devices.find((x) => x.name === device)
  if (!d) return ''
  return d.bindings.filter((b) => memeCmd(b, cmd)).map((b) => b.code).join(', ')
}

/// Reconstruit la table complete : les autres peripheriques sont preserves
/// tels quels, seul le peripherique courant est reecrit depuis le tableau.
/// `codes` est indexe comme `ACTIONS`.
export function collect(table: BindingTable, device: string, codes: string[]): BindingTable {
  const devices = table.devices.filter((d) => d.name !== device)
  const bindings: Binding[] = []
  ACTIONS.forEach((a, i) => {
    const brut = (codes[i] ?? '').trim()
    if (!brut) return
    for (const part of brut.split(',')) {
      const code = Number.parseInt(part.trim(), 10)
      if (!Number.isNaN(code)) bindings.push({ code, ...a.cmd })
    }
  })
  if (device) devices.push({ name: device, bindings })
  return { devices }
}

/// Serialisation TOML en miroir du format lu par `presets::parse_preset`
/// (crates/ritornello-plugin-generic-input/src/presets.rs) : un bloc
/// `[[bindings]]` par binding, `arg` seulement s'il est present. Toute
/// evolution du format cote Rust doit etre repercutee ici, sous peine de
/// fichiers exportes que le serveur refuserait.
export function presetToml(bindings: Binding[]): string {
  return bindings
    .map((b) => {
      let bloc = `[[bindings]]\ncode = ${b.code}\ncmd = "${b.cmd}"\n`
      if (b.arg !== undefined && b.arg !== null) bloc += `arg = ${b.arg}\n`
      return bloc
    })
    .join('\n')
}

export function sanitiseDeviceName(name: string): string {
  return name.replace(/[^a-zA-Z0-9_-]+/g, '_')
}
```

- [ ] **Step 6: Écrire le composant**

`crates/ritornello-plugin-generic-input/ui/src/InputAdmin.vue` :

```vue
<script setup lang="ts">
import {
  api, Button, createT, Input, type Catalog,
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from '@ritornello/ui'
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { ACTIONS, codesFor, collect, presetToml, sanitiseDeviceName, type BindingTable } from './preset-toml'

const props = defineProps<{ catalog: Catalog }>()
const t = computed(() => createT(props.catalog))

const SONDAGE_MS = 300
const DELAI_MS = 10_000

interface Data {
  devices: string[]
  bindings: BindingTable
  presets: string[]
  learning: { captured: number | null } | null
}

const data = ref<Data>({ devices: [], bindings: { devices: [] }, presets: [], learning: null })
const device = ref('')
const preset = ref('')
const codes = ref<string[]>(ACTIONS.map(() => ''))
const message = ref('')
const apprend = ref(false)
let timer: ReturnType<typeof setInterval> | null = null

function remplirCodes() {
  codes.value = ACTIONS.map((a) => (device.value ? codesFor(data.value.bindings, device.value, a.cmd) : ''))
}

async function recharger() {
  try {
    data.value = await api.get<Data>('./api/data')
    if (!data.value.devices.includes(device.value)) device.value = data.value.devices[0] ?? ''
    if (!preset.value) preset.value = data.value.presets[0] ?? ''
    remplirCodes()
    message.value = device.value ? '' : t.value('no_device')
  } catch (e) {
    message.value = t.value('load_error') + (e as Error).message
  }
}

onMounted(recharger)
onUnmounted(() => stopTimer())
watch(device, remplirCodes)

function stopTimer() {
  if (timer) clearInterval(timer)
  timer = null
}

async function arreterApprentissage(texte: string) {
  stopTimer()
  apprend.value = false
  await api.put('./api/data', { op: 'cancel_learn' })
  message.value = texte
}

/// Apprentissage : le plugin capture la prochaine touche du peripherique, la
/// vue sonde `GetData` jusqu'a la voir arriver. Meme mecanique que l'ancienne
/// page — sondage court, delai de 10 s, annulation explicite.
async function apprendre(i: number) {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  if (timer) await arreterApprentissage('')
  const err = await api.put('./api/data', { op: 'learn', device: device.value })
  if (err) {
    message.value = err
    return
  }
  apprend.value = true
  message.value = t.value('learning_msg')
  const echeance = Date.now() + DELAI_MS
  timer = setInterval(async () => {
    if (Date.now() > echeance) {
      await arreterApprentissage(t.value('learn_timeout'))
      return
    }
    let d: Data
    try {
      d = await api.get<Data>('./api/data')
    } catch {
      return // une lecture ratee ne doit pas interrompre le sondage
    }
    const c = d.learning?.captured
    if (c !== null && c !== undefined) {
      codes.value[i] = String(c)
      await arreterApprentissage('')
    }
  }, SONDAGE_MS)
}

async function enregistrer() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const table = collect(data.value.bindings, device.value, codes.value)
  const err = await api.put('./api/data', { op: 'save', bindings: table })
  if (err) {
    message.value = t.value('save_error') + err
    return
  }
  data.value.bindings = table
  message.value = t.value('saved')
}

async function rafraichir() {
  await api.put('./api/data', { op: 'rescan' })
  await recharger()
}

async function chargerPreset() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const err = await api.put('./api/data', {
    op: 'load_preset',
    device: device.value,
    preset: preset.value,
  })
  if (err) {
    message.value = err
    return
  }
  await recharger()
}

/// Import : le fichier est lu en texte cote navigateur, puis parse et valide
/// cote serveur (`import_preset`) — exactement comme `load_preset` mais sans
/// passer par /etc/ritornello/input-presets. Rien n'est persiste avant un
/// « Enregistrer » explicite.
async function importer(e: Event) {
  const input = e.target as HTMLInputElement
  const fichier = input.files?.[0]
  input.value = '' // permet de reimporter le meme fichier ensuite
  if (!fichier) return
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  try {
    const contenu = await fichier.text()
    const err = await api.put('./api/data', {
      op: 'import_preset',
      device: device.value,
      content: contenu,
    })
    if (err) {
      message.value = err
      return
    }
    await recharger()
  } catch (err) {
    message.value = t.value('load_error') + (err as Error).message
  }
}

function exporter() {
  if (!device.value) {
    message.value = t.value('no_device')
    return
  }
  const d = data.value.bindings.devices.find((x) => x.name === device.value)
  const blob = new Blob([presetToml(d ? d.bindings : [])], { type: 'application/toml' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `ritornello-bindings-${sanitiseDeviceName(device.value)}.toml`
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center gap-2">
      <label class="text-sm text-muted-foreground">{{ t('device_label') }}</label>
      <Select v-model="device">
        <SelectTrigger data-device-select class="min-w-64"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="d in data.devices" :key="d" :value="d">{{ d }}</SelectItem>
        </SelectContent>
      </Select>
      <Button variant="secondary" data-refresh @click="rafraichir">{{ t('btn_refresh') }}</Button>
    </div>

    <table class="w-full text-sm">
      <thead class="text-muted-foreground">
        <tr>
          <th class="text-left font-normal">{{ t('col_action') }}</th>
          <th class="text-left font-normal">{{ t('col_code') }}</th>
          <th class="w-24" /><th class="w-24" />
        </tr>
      </thead>
      <tbody>
        <tr v-for="(a, i) in ACTIONS" :key="a.key" data-action-row class="border-t border-border">
          <td class="py-1">{{ t(a.key) }}</td>
          <td class="py-1 pr-2"><Input v-model="codes[i]" inputmode="numeric" /></td>
          <td><Button variant="secondary" size="sm" data-learn @click="apprendre(i)">{{ t('btn_learn') }}</Button></td>
          <td><Button variant="ghost" size="sm" data-clear @click="codes[i] = ''">{{ t('btn_clear') }}</Button></td>
        </tr>
      </tbody>
    </table>

    <div class="flex flex-wrap items-center gap-2">
      <label class="text-sm text-muted-foreground">{{ t('preset_label') }}</label>
      <Select v-model="preset">
        <SelectTrigger class="min-w-40"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem v-for="p in data.presets" :key="p" :value="p">{{ p }}</SelectItem>
        </SelectContent>
      </Select>
      <Button variant="secondary" @click="chargerPreset">{{ t('btn_load_preset') }}</Button>
      <label class="cursor-pointer rounded-md border border-border px-3 py-2 text-sm">
        {{ t('btn_import') }}
        <input type="file" accept=".toml" data-import class="hidden" @change="importer" />
      </label>
      <Button variant="secondary" @click="exporter">{{ t('btn_export') }}</Button>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <Button data-save @click="enregistrer">{{ t('btn_save') }}</Button>
      <Button v-if="apprend" variant="ghost" @click="arreterApprentissage('')">
        {{ t('btn_cancel') }}
      </Button>
      <span class="text-sm text-muted-foreground">{{ message }}</span>
    </div>
  </div>
</template>
```

`crates/ritornello-plugin-generic-input/ui/src/index.ts` :

```ts
import { UI_CONTRACT } from '@ritornello/ui'
import InputAdmin from './InputAdmin.vue'
import './ui.css'

/// Version du contrat contre laquelle ce module est compilé. Le shell la
/// compare à la sienne avant de monter le composant.
export const contract = UI_CONTRACT
export default InputAdmin
```

- [ ] **Step 7: Lancer les tests et le build — succès attendu**

Run : `npm test --workspaces && npm run build --workspaces && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected : SUCCÈS.

- [ ] **Step 8: Commit**

```bash
git add crates/ritornello-plugin-generic-input package-lock.json
git commit -m "feat(generic-input): IHM en module ESM (21 actions, apprentissage, presets, import/export)"
```

---

### Task 13: Parcours Playwright de bout en bout

Vérifie ce qu'aucun test unitaire ne peut voir : l'import map résolue par un
vrai navigateur, le thème appliqué en variables CSS **calculées**, et le
chargement dynamique d'un module de plugin servi par IPC.

**Prérequis :** `mpv` installé (le cœur le lance et s'arrête s'il meurt — voir
la section « Développement » du README).

**Files:**
- Create: `web/app/playwright.config.ts`, `web/app/e2e/serve.mjs`, `web/app/e2e/parcours.spec.ts`
- Modify: `web/app/package.json`, `package-lock.json`, `.gitignore`

**Interfaces:**
- Consumes: le workspace bâti (Tasks 1 à 12) et les binaires `cargo build --workspace`.
- Produces: script `npm run e2e -w app`.

- [ ] **Step 1: Installer Playwright**

```bash
npm i -w app -D @playwright/test
npx playwright install chromium
```

Ajouter à `web/app/package.json` : `"e2e": "playwright test"`.
Ajouter à `.gitignore` : `/web/app/test-results`, `/web/app/playwright-report`
(déjà posés en Task 1).

- [ ] **Step 2: Écrire le harnais de lancement**

`web/app/e2e/serve.mjs` — démarre une instance jetable du cœur, avec le plugin
radio et le plugin generic-input, sur le modèle de la section
« Développement » du README :

```js
// Lance un coeur jetable pour les parcours Playwright : repertoire d'etat
// temporaire, port dedie, les deux plugins a IHM declares. Volontairement
// proche de la configuration de developpement du README.
import { spawn } from 'node:child_process'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const racine = process.cwd().replace(/[\\/]web[\\/]app$/, '')
const dir = mkdtempSync(join(tmpdir(), 'ritornello-e2e-'))

writeFileSync(
  join(dir, 'plugins.toml'),
  `[[plugin]]
name = "radio"
kind = "source"
exec = "${racine}/target/debug/ritornello-plugin-radio"
admin = true

[[plugin]]
name = "generic-input"
kind = "input"
exec = "${racine}/target/debug/ritornello-plugin-generic-input"
admin = true
`,
)
writeFileSync(
  join(dir, 'stations.toml'),
  '[[stations]]\nname = "FIP"\nurl = "http://icecast.radiofrance.fr/fip-midfi.mp3"\npreset = 1\n',
)

const enfant = spawn(`${racine}/target/debug/ritornello-core`, {
  stdio: 'inherit',
  env: {
    ...process.env,
    RITORNELLO_HTTP: '127.0.0.1:8099',
    RITORNELLO_PLUGINS: join(dir, 'plugins.toml'),
    RITORNELLO_STATE: join(dir, 'state.json'),
    RITORNELLO_RUNTIME_DIR: dir,
    RITORNELLO_MPV_SOCKET: join(dir, 'mpv.sock'),
    RITORNELLO_RADIO_STATIONS: join(dir, 'stations.toml'),
    RITORNELLO_RADIO_STATE: join(dir, 'plugin-radio.json'),
    RITORNELLO_INPUT_BINDINGS: join(dir, 'input-bindings.toml'),
    RITORNELLO_INPUT_PRESETS: `${racine}/deploy/input-presets`,
  },
})
process.on('SIGTERM', () => enfant.kill('SIGTERM'))
process.on('exit', () => enfant.kill('SIGTERM'))
```

`web/app/playwright.config.ts` :

```ts
import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  use: { baseURL: 'http://127.0.0.1:8099', ...devices['Desktop Chrome'] },
  // Le binaire doit exister : `cargo build --workspace` fait partie de la
  // chaine de build (voir deploy/build.sh).
  webServer: {
    command: 'node e2e/serve.mjs',
    url: 'http://127.0.0.1:8099/api/status',
    reuseExistingServer: false,
    timeout: 60_000,
  },
})
```

- [ ] **Step 3: Écrire les parcours**

`web/app/e2e/parcours.spec.ts` :

```ts
import { expect, test } from '@playwright/test'

const variable = (page: import('@playwright/test').Page, nom: string) =>
  page.evaluate(
    (n) => getComputedStyle(document.documentElement).getPropertyValue(n).trim(),
    nom,
  )

test('navigation entre l’accueil, le statut et les pages de plugin', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-preset-button="1"]')).toBeVisible()
  await page.goto('/status')
  await expect(page.getByText('radio')).toBeVisible()
  // Page de plugin : le module ESM est charge dynamiquement et resolu par
  // l'import map — c'est ce qu'aucun test unitaire ne peut verifier.
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  await page.goto('/plugins/generic-input/')
  await expect(page.locator('[data-action-row]')).toHaveCount(21)
})

test('une seule instance de Vue sert le shell et les modules de plugin', async ({ page }) => {
  const requetes: string[] = []
  page.on('request', (r) => requetes.push(new URL(r.url()).pathname))
  await page.goto('/plugins/radio/')
  await expect(page.locator('[data-save]')).toBeVisible()
  expect(requetes.filter((p) => p === '/assets/vue.js')).toHaveLength(1)
  expect(requetes).toContain('/plugins/radio/ui.js')
  expect(requetes).toContain('/plugins/radio/ui.css')
})

test('bascule clair/sombre, appliquée et persistée', async ({ page }) => {
  await page.goto('/')
  const clair = await variable(page, '--background')
  await page.getByLabel('toggle theme mode').click()
  await expect.poll(() => variable(page, '--background')).not.toBe(clair)
  const sombre = await variable(page, '--background')
  // Persistance cote serveur : un rechargement doit conserver le mode.
  await page.reload()
  await expect.poll(() => variable(page, '--background')).toBe(sombre)
  expect(await page.evaluate(() => document.documentElement.classList.contains('dark'))).toBe(true)
})

test('choix d’un thème dans la popin, appliqué et persisté', async ({ page }) => {
  await page.goto('/')
  await page.getByLabel('pick theme').click()
  await page.locator('[data-preset="vercel"]').click()
  const primaire = await variable(page, '--primary')
  await page.reload()
  await expect.poll(() => variable(page, '--primary')).toBe(primaire)
  await page.getByLabel('pick theme').click()
  await expect(page.locator('[data-preset="vercel"]')).toHaveAttribute('data-active', 'true')
})

test('la popin liste les 42 thèmes et les filtre', async ({ page }) => {
  await page.goto('/')
  await page.getByLabel('pick theme').click()
  await expect(page.locator('[data-preset]')).toHaveCount(42)
  await page.getByPlaceholder('filter').fill('northern')
  await expect(page.locator('[data-preset]')).toHaveCount(1)
})

test('ajout et enregistrement d’une station, relus depuis l’API', async ({ page, request }) => {
  await page.goto('/plugins/radio/')
  await page.locator('[data-add]').click()
  const lignes = page.locator('[data-station-name]')
  await lignes.last().fill('Test E2E')
  await page.locator('[data-station-url]').last().fill('http://exemple.test/flux.mp3')
  await page.locator('[data-save]').click()
  const data = await (await request.get('/plugins/radio/api/data')).json()
  expect(data.stations.map((s: { name: string }) => s.name)).toContain('Test E2E')
  // Numerotation par position : la station ajoutee prend la presélection 2.
  expect(data.stations.find((s: { name: string }) => s.name === 'Test E2E').preset).toBe(2)
})

test('apprentissage de touche : la vue atteint un état défini', async ({ page }) => {
  await page.goto('/plugins/generic-input/')
  const premiere = page.locator('[data-action-row]').first()
  await premiere.locator('[data-learn]').click()
  // Deux issues sont legitimes selon que l'environnement expose ou non un
  // peripherique evdev lisible, et les deux sont des etats definis :
  //  - aucun peripherique  -> « No input device detected »
  //  - apprentissage lance -> « Press a key on the device… »
  // On assert sur cet ensemble ferme de messages (valeurs de
  // crates/ritornello-plugin-generic-input/src/locales/en.toml), et non sur
  // « un texte quelconque » : un test qui accepte n'importe quoi ne prouve
  // rien.
  await expect(
    page.getByText(/No input device detected|Press a key on the device/),
  ).toBeVisible()
})
```

- [ ] **Step 4: Lancer les parcours — succès attendu**

Run : `npm run build --workspaces && cargo build --workspace && npm run e2e -w app`
Expected : SUCCÈS, 7 parcours.

En cas d'échec sur le harnais plutôt que sur l'IHM (binaire absent, `mpv`
manquant), le diagnostic est dans la sortie du cœur, reprise en direct
(`stdio: 'inherit'`).

- [ ] **Step 5: Commit**

```bash
git add web/app package-lock.json .gitignore
git commit -m "test(web): parcours playwright (navigation, themes, stations, apprentissage)"
```

---

### Task 14: Chaîne de build, README et vérification finale

Formalise la séquence en trois étapes, documente le nouveau prérequis Node et
la manière dont un plugin tiers livre son IHM, puis vérifie l'ensemble —
y compris la cross-compilation ARM, qui est la raison d'être du découplage
entre le build npm et les builds cargo.

**Files:**
- Create: `deploy/build.sh`
- Modify: `README.md`

**Interfaces:**
- Consumes: tout ce qui précède.
- Produces: `deploy/build.sh` comme procédure de référence.

- [ ] **Step 1: Écrire la chaîne de build**

`deploy/build.sh` (exécutable : `chmod +x`) :

```bash
#!/usr/bin/env bash
# Chaine de build complete de ritornello.
#
# Le build npm ne tourne **qu'une fois** : les livrables qu'il depose sont lus
# par `include_str!`/`rust-embed` a la compilation, donc les deux etapes cargo
# les consomment tels quels. C'est ce qui permet a `cross` de fonctionner avec
# une image Docker sans Node.
set -euo pipefail

TARGET="${TARGET:-armv7-unknown-linux-gnueabihf}"

echo "== 1/3 IHM web (npm) =="
npm ci
npm run build --workspaces

echo "== 2/3 build natif (x86_64) =="
cargo build --workspace

echo "== 3/3 cross-compilation ($TARGET) =="
cross build --release --workspace --target "$TARGET"

echo "OK"
```

- [ ] **Step 2: Mettre à jour le README**

Dans la section **Compiler**, avant les commandes cargo :

```markdown
L'interface web est une SPA (Vue 3 + shadcn-vue) embarquée dans le binaire du
cœur : **Node 20+** est donc un prérequis de développement, là où `cargo`
suffisait. La procédure de référence est `deploy/build.sh`, qui enchaîne
toujours les trois étapes dans cet ordre :

    ./deploy/build.sh                 # npm, puis cargo x86_64, puis cross ARM
    TARGET=aarch64-unknown-linux-gnu ./deploy/build.sh

Le build npm ne tourne qu'une fois : son livrable est lu à la compilation par
les deux étapes cargo. C'est ce qui permet à `cross` de fonctionner avec une
image Docker sans Node.

Un `cargo build` lancé seul, sans avoir construit l'IHM, **réussit** : un
bouchon est embarqué à la place, et la page servie invite à lancer
`npm run build --workspaces`. Ce n'est pas une panne. Les tests
(`cargo test --workspace`) restent verts dans cette situation ; côté
navigateur, `npm test --workspaces` couvre l'IHM et `npm run e2e -w app` les
parcours complets (Playwright, chromium).
```

Ajouter une section **Thème** après « Internationalisation » :

```markdown
## Thème

L'interface propose une bascule **clair/sombre** et un sélecteur ouvrant une
popin avec les **42 thèmes** de [tweakcn](https://tweakcn.com) (Apache-2.0).
C'est un réglage **de l'appareil**, comme la langue : il est persisté dans
`state.json` (champs `theme` et `mode`) et s'applique donc à tous les
navigateurs qui consultent l'interface. Défaut : `northern-lights`, mode clair.

Les polices déclarées par les thèmes sont chargées depuis un CDN — la seule
ressource externe de l'interface. Hors ligne, l'affichage retombe sur la police
système sans autre conséquence.

Régénérer les presets depuis l'amont :
`cd web/kit && node scripts/fetch-presets.mjs`.
```

Dans la section **Plugins**, ajouter :

```markdown
### IHM d'un plugin

Un plugin qui déclare `admin = true` peut livrer sa propre interface, sans
qu'une ligne du cœur change. Il répond à trois requêtes du protocole d'admin :

- `GetAsset("ui.js")` → un **module ESM** exportant `contract` (la version du
  contrat, voir `web/kit/src/contract.ts`) et, par défaut, un composant Vue ;
- `GetAsset("ui.css")` → la feuille de style du module (sa propre passe
  Tailwind, important : le CSS du cœur ne contient que les classes qu'il voit) ;
- `GetCatalog` → son catalogue i18n à plat, que la vue consomme via `t()`.

Le module importe `vue` et `@ritornello/ui` **sans les embarquer** : le shell
les fournit par une import map, donc une seule instance de Vue et un seul jeu
de composants servent tout le monde. Un contrat incompatible est signalé dans
l'interface plutôt que de casser la page.

L'ESM natif ne demande aucune compilation : un plugin simple peut livrer un
`ui.js` **écrit à la main**. Les deux plugins livrés utilisent un build Vite
(voir `crates/ritornello-plugin-radio/ui/`) pour bénéficier des `.vue` et de
TypeScript — c'est un choix de confort, pas une exigence.
```

Enfin : remplacer les mentions de `http://<hôte>:8080/status` comme point
d'entrée par `http://<hôte>:8080/` (la télécommande est désormais sur
l'accueil ; `/status` reste valide et sert le diagnostic), et retirer la phrase
« La page de statut (`/status`) embarque une télécommande » de la section
**Télécommande web** au profit de l'accueil.

- [ ] **Step 3: Vérification complète du workspace**

```bash
npm ci
npm run build --workspaces
npm test --workspaces
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
npm run e2e -w app
```

Attendu : tout vert.

- [ ] **Step 4: Vérifier qu'aucun nom de plugin n'a fui dans le cœur ni le shell**

```bash
grep -rn --include=*.rs -e '"radio"' -e '"generic-input"' crates/ritornello-core/src \
  | grep -v '#\[cfg(test)\]' | grep -v 'mod tests' || echo "aucune fuite hors tests"
grep -rn -e 'radio' -e 'generic-input' web/app/src || echo "aucune fuite dans le shell"
```

Attendu : aucune occurrence hors des tests du cœur (où `"radio"` sert de nom de
plugin factice). C'est la propriété d'architecture que ce chantier devait
préserver : un plugin tiers apparaît sans modifier le cœur.

- [ ] **Step 5: Vérifier le chemin sans Node (celui que `cross` empruntera)**

```bash
git stash push -u -m "verif-sans-dist" -- web/app/dist crates/*/ui/dist 2>/dev/null || true
rm -rf web/app/dist crates/ritornello-plugin-radio/ui/dist crates/ritornello-plugin-generic-input/ui/dist
cargo build --workspace 2>&1 | grep -c "bouchon"   # attendu : 3 avertissements
cargo test --workspace                              # doit rester vert
npm run build --workspaces                          # remet les vrais livrables
```

- [ ] **Step 6: Cross-compilation ARM**

```bash
cargo install cross --locked   # si absent
cross build --release --workspace --target armv7-unknown-linux-gnueabihf
```

Attendu : SUCCÈS **sans** que Node soit présent dans l'image — c'est la
vérification qui valide toute la conception du build. Noter la taille du
binaire du cœur (`ls -lh target/armv7-unknown-linux-gnueabihf/release/ritornello-core`) :
l'IHM embarquée doit rester de l'ordre de quelques centaines de Ko.

- [ ] **Step 7: Commit**

```bash
chmod +x deploy/build.sh
git add deploy/build.sh README.md
git commit -m "docs: chaine de build en trois etapes, section theme et IHM d'un plugin tiers"
```

---

## Notes d'exécution

**Ordre des tasks.** Les Tasks 1 à 4 sont purement JS et n'affectent pas le
workspace Rust. La Task 10 est le point de non-retour : elle retire `GetPage`
et laisse les IHM de plugin momentanément indisponibles jusqu'aux Tasks 11 et
12. Ne rien déployer entre la Task 10 et la Task 12.

**Ce qui disparaît en route** — utile pour relire les diffs :

| Élément retiré | Task | Pourquoi |
|---|---|---|
| `status_page`, `escape_html` (cœur) | 9 | le cœur ne génère plus de HTML métier |
| route `GET /status` (cœur) | 9 | servie par le repli SPA |
| `AdminReq::GetPage`, `AdminResult::Page` | 10 | remplacés par `GetAsset` |
| `AdminPlugin::page`, `AdminClient::get_page`, `AdminBackend::page` | 10 | idem |
| `PAGE_KEYS` et la substitution `{{clé}}` (2 plugins) | 10 | les catalogues sont des données, plus du source |
| garde-fou `aucune_valeur_ne_contient_un_caractere_dangereux…` | 10 | plus de substitution, donc plus de caractère dangereux |
| `src/index.html` (2 plugins) | 10 | remplacés par `ui/dist/{ui.js,ui.css}` |

**Ce qui est explicitement conservé** : le test de parité des clés en/fr dans
les deux plugins, les tests de validation et de persistance des stations et des
bindings, la sémantique de `PUT` (204 / 422 + `{error}`), les URL `/status` et
`/plugins/<nom>/`, et le canal unique `/api/command` partagé avec les plugins
Input.

