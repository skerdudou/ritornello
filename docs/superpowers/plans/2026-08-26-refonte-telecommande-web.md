# Refonte visuelle de la télécommande web — plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transformer la page d'accueil de la SPA en une vraie télécommande — pochette et morceau au centre, transport en icônes, curseurs tactiles, présélections nommées, barre d'onglets fixe sur téléphone — sans toucher au protocole ni au thème.

**Architecture:** Une seule `HomeView`, un seul flux SSE, deux mises en page par points de rupture Tailwind (`md`). Le cœur gagne une route de lecture `GET /api/presets` qui sert le `Catalogue` qu'il tient déjà. Le kit gagne un composant `Slider` (reka-ui) partagé par la barre de progression et le volume. La page d'accueil se découpe en composants (`PlayerCard`, `Transport`, `Volume`, `GrillePresets`, `NavBasse`, `PluginsView`) qui n'emploient que les jetons du thème.

**Tech Stack:** Rust (axum, tokio `watch`), Vue 3.5 `<script setup>` TS, vue-router 5, reka-ui 2.10 (`SliderRoot`), Tailwind v4 (classes utilitaires, aucun CSS custom), `@radix-icons/vue`, vitest + @vue/test-utils, Playwright.

**Spec:** `docs/superpowers/specs/2026-08-26-refonte-telecommande-web-design.md`

## Global Constraints

- **Aucune couleur, police ni rayon en dur** : uniquement les classes Tailwind mappées sur les jetons (`bg-primary`, `text-muted-foreground`, `bg-muted`, `border-border`, `bg-card`, `rounded-md`…). Les 42 palettes et le mode sombre doivent rester intacts.
- **Aucun changement de protocole ni de charge utile existante.** Une seule addition côté cœur : `GET /api/presets`.
- **Les marqueurs `data-*` existants sont conservés** (`data-player`, `data-source`, `data-volume`, `data-now-playing`, `data-pochette`, `data-pochette-repli`, `data-titre`, `data-artiste`, `data-album`, `data-origin`, `data-cover-origin`, `data-player-preset`, `data-player-preset-name`, `data-player-status`, `data-standby`, `data-progression`, `data-barre`, `data-remplissage`, `data-position`, `data-duree-totale`, `data-preset-button`, `data-preset-active`, `data-preset-prev`, `data-preset-next`, `data-preset-count`, `data-remote-power`, `data-remote-source`, `data-remote-command`). Les e2e s'y accrochent.
- **Icônes** : `@radix-icons/vue` (vérifié disponibles : `PlayIcon`, `PauseIcon`, `StopIcon`, `TrackPreviousIcon`, `TrackNextIcon`, `SpeakerLoudIcon`, `SpeakerOffIcon`, `ChevronLeftIcon`, `ChevronRightIcon`, `MixerHorizontalIcon`, `CubeIcon`, `ActivityLogIcon`, `LoopIcon`) ; **Radix n'a ni Power ni Eject** → deux SVG maison au même format 15×15 (Tâche 8).
- **Copie i18n** : toute clé nouvelle existe dans `crates/ritornello-core/src/locales/en.toml` **et** `deploy/locales/core/fr.toml` (test Rust `parite_des_cles_entre_len_embarque_et_le_pack_fr`, et test web `i18nKeysUsed.test.ts`).
- **Cibles tactiles ≥ 44 px** sur téléphone.
- **Ordre des greffons** = ordre de `/api/status` (`useGreffons().admins`), jamais trié ni priorisé.
- **Commits** : un par tâche, message à la convention du dépôt (`feat(web): …`, `feat(core): …`, `docs: …`, français, pas d'accents dans le sujet).

## Prérequis d'environnement (à lire avant la Tâche 1)

- **Rust ne se compile que sous WSL** : `wsl.exe -e bash -lc 'cd /mnt/c/projets/perso/ritornello/.claude/worktrees/chantier-refonte-telecommande && cargo test -p ritornello-core'`. La Tâche 1 ajoute un champ à `AppState` : lancer aussi `cargo build --workspace` (les littéraux d'`AppState` vivent dans quatre fichiers, `cargo test -p` n'en voit pas tous — voir la liste dans la tâche).
- **Tests web depuis un worktree** : `web/app/node_modules` du worktree n'a pas les jonctions. Créer, **une seule fois**, depuis PowerShell :
  `New-Item -ItemType Junction -Path web\app\node_modules\vue-router -Target C:\projets\perso\ritornello\web\app\node_modules\vue-router` et de même pour `web\app\node_modules\@ritornello\ui` → `C:\projets\perso\ritornello\web\kit`. **Surtout pas** pour `vite`. Puis `cd web/app && npx vitest run <fichier>`. Pour le kit : `cd web/kit && npx vitest run`.
- **e2e** : `cargo build --workspace` sous WSL d'abord (les binaires des greffons dans `target/debug` étaient plus vieux que le cœur au 2026-08-26 — `unknown variant ListPresets` au démarrage), puis `cd web/app && npx playwright test`.
- Vérifier avant tout : `npx vitest run` dans `web/app` et `web/kit` passe au vert sur la branche de départ.

## Carte des fichiers

| Fichier | Rôle |
|---|---|
| `crates/ritornello-core/src/status.rs` | `AppState.catalogue`, route `GET /api/presets`, test |
| `crates/ritornello-core/src/main.rs:1240`, `admin.rs:232`, `core.rs:5858` | littéraux `AppState` à compléter |
| `web/kit/src/components/ui/slider/{index.ts,Slider.vue}` + `web/kit/src/index.ts` | composant `Slider` du kit |
| `web/kit/src/index.test.ts` | test du `Slider` |
| `web/app/src/views/remoteCommands.ts` | `REMOTE_TRANSPORT`, `REMOTE_TRANSPORT_SECONDAIRE`, `REMOTE_MUTE`, `masquee`, `indisponible` |
| `web/app/src/types.ts` | `Playback`, `PlayerPayload.playback`, `PresetsPayload` |
| `web/app/src/composables/usePresets.ts` | charge `/api/presets`, `nomDe(source, n)` |
| `web/app/src/components/BarreProgression.vue` | curseur tactile sur `Slider`, trois états |
| `web/app/src/components/Volume.vue` | curseur de volume + Muet |
| `web/app/src/components/Transport.vue` | |◀ ▶ ▶| ■ ⏏ en icônes |
| `web/app/src/components/icones/{IconeVeille,IconeEjecter}.vue` | les deux SVG que Radix n'a pas |
| `web/app/src/components/GrillePresets.vue` | tuiles nommées + pagination (logique extraite de `HomeView`) |
| `web/app/src/components/PlayerCard.vue` | pochette au centre, pastille source, surligne présélection, slots `actions` et `commandes` |
| `web/app/src/views/HomeView.vue` | assemblage, deux colonnes à partir de `md` |
| `web/app/src/components/NavBasse.vue` | barre d'onglets basse, 4 entrées fixes |
| `web/app/src/views/PluginsView.vue` + `router.ts` | liste `/plugins/` |
| `web/app/src/App.vue` | nav du haut masquée sous `md`, `NavBasse`, `pb` du `main` |
| `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml` | clés ajoutées / retirées |
| `web/app/e2e/telephone.spec.ts`, `web/app/playwright.config.ts` | parcours au viewport téléphone |
| `web/app/scripts/captures.mjs`, `docs/captures/*.png`, `docs/interface.md` | documentation |

---

### Task 1: Route `GET /api/presets` (cœur)

**Files:**
- Modify: `crates/ritornello-core/src/status.rs:130-200` (struct `AppState`, `router`), `:865-930` (aides de test)
- Modify: `crates/ritornello-core/src/main.rs:1240-1256` (littéral `app_state_squelette`)
- Modify: `crates/ritornello-core/src/admin.rs:232` (littéral de test)
- Modify: `crates/ritornello-core/src/core.rs:5858` (littéral de test)
- Test: `crates/ritornello-core/src/status.rs` (module `tests`)

**Interfaces:**
- Consomme : `ritornello_proto::{Catalogue, SourceCatalogue, Preset}` (réexportés à la racine du crate, vérifié dans `lib.rs:10,16`) ; `catalogue_rx: watch::Receiver<Catalogue>` créé en `main.rs:866`.
- Produit : `GET /api/presets` → JSON `{"sources":[{"name":"radio","presets":[{"index":1,"name":"FIP"}]},{"name":"cd"}]}` — une source sans liste **n'a pas** de champ `presets` (`skip_serializing_if = "Vec::is_empty"` sur `SourceCatalogue`).

- [ ] **Step 1 : le test qui échoue**

Dans le module `tests` de `status.rs`, après `api_status_liste_les_plugins` :

```rust
    /// Les tuiles de la télécommande web lisent ici le nom des présélections :
    /// le cœur tient déjà ce catalogue pour les afficheurs, la route ne fait
    /// que le rendre lisible en HTTP. Une source qui n'énumère pas n'a pas de
    /// champ `presets` — la page retombe alors sur les numéros seuls.
    #[tokio::test]
    async fn api_presets_sert_le_catalogue_courant() {
        use ritornello_proto::{Catalogue, Preset, SourceCatalogue};
        let (tx, rx) = tokio::sync::watch::channel(Catalogue::default());
        let app = router(AppState { catalogue: rx, ..app_state() });
        tx.send(Catalogue {
            sources: vec![
                SourceCatalogue {
                    name: "radio".into(),
                    presets: vec![Preset { index: 1, name: "FIP".into() }],
                },
                SourceCatalogue { name: "cd".into(), presets: vec![] },
            ],
        })
        .unwrap();
        let resp = app.oneshot(Request::get("/api/presets").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["sources"][0]["name"], "radio");
        assert_eq!(v["sources"][0]["presets"][0], serde_json::json!({ "index": 1, "name": "FIP" }));
        assert_eq!(v["sources"][1]["name"], "cd");
        assert_eq!(v["sources"][1].get("presets"), None, "une source qui n'énumère pas n'a pas de champ presets");
    }
```

- [ ] **Step 2 : vérifier l'échec**

Run (WSL) : `cargo test -p ritornello-core api_presets_sert_le_catalogue_courant`
Expected : erreur de compilation `no field 'catalogue' on type AppState`.

- [ ] **Step 3 : le champ, la route, le handler**

Dans `AppState` (après `player`) :

```rust
    /// Catalogue des sources et de leurs présélections nommées, tel que le cœur
    /// le diffuse aux afficheurs (`Core::catalogue`). Le même `watch` que celui
    /// des greffons Display : la route lit la dernière valeur, rien n'est
    /// sondé côté cœur, et la liste ne change qu'à l'annonce ou au départ
    /// d'une source.
    pub catalogue: tokio::sync::watch::Receiver<ritornello_proto::Catalogue>,
```

Dans `router`, après la ligne `/api/player` :

```rust
        .route("/api/presets", get(presets_json))
```

Après `status_json` :

```rust
/// Les présélections nommées de chaque source, pour les tuiles de la
/// télécommande web. Une lecture de la valeur courante, pas un flux : la page
/// la recharge au changement de source (la trame SSE le lui dit), et c'est
/// assez — voir la spec, décision 6.
async fn presets_json(State(state): State<AppState>) -> Json<ritornello_proto::Catalogue> {
    Json(state.catalogue.borrow().clone())
}
```

- [ ] **Step 4 : compléter les cinq littéraux `AppState`**

`status.rs` — dans `app_state()` **et** dans `app_state_with_audio()`, après `player: player_inerte(),` :

```rust
            catalogue: tokio::sync::watch::channel(ritornello_proto::Catalogue::default()).1,
```

`main.rs:1256`, après `player: etat_rx.clone(),` (le `catalogue_rx` de la ligne 866 est dans la portée) :

```rust
            catalogue: catalogue_rx.clone(),
```

`admin.rs:232` et `core.rs:5858` : même ligne que dans `status.rs`. Si l'un des deux construit son `AppState` par `..app_state()`, rien à ajouter là.

- [ ] **Step 5 : vérifier le vert, tout le workspace**

Run (WSL) : `cargo test -p ritornello-core api_presets` puis `cargo build --workspace && cargo test -p ritornello-core && cargo clippy --workspace --all-targets -- -D warnings`
Expected : le test passe ; aucun `missing field catalogue` ailleurs ; clippy muet.

- [ ] **Step 6 : commit**

```bash
git add crates/ritornello-core/src/status.rs crates/ritornello-core/src/main.rs crates/ritornello-core/src/admin.rs crates/ritornello-core/src/core.rs
git commit -m "feat(core): GET /api/presets sert le catalogue des preselections nommees"
```

---

### Task 2: Composant `Slider` du kit

**Files:**
- Create: `web/kit/src/components/ui/slider/index.ts`, `web/kit/src/components/ui/slider/Slider.vue`
- Modify: `web/kit/src/index.ts`
- Test: `web/kit/src/index.test.ts`

**Interfaces:**
- Produit : `<Slider :model-value="[n]" :min :max :step :disabled :aria-label @update:model-value="(v: number[]) => …" @value-commit="(v: number[]) => …" />`. Une seule poignée. Zone de contact 44 px (`py-[19px]` autour d'une piste `h-1.5`). `update:modelValue` suit le doigt ; `valueCommit` part **une fois** au relâchement (ou à chaque pas clavier). Rendu `role="slider"` sur la poignée, `aria-valuenow/min/max`.

- [ ] **Step 1 : le test qui échoue**

Dans `web/kit/src/index.test.ts`, ajouter à l'import `Slider` et le test :

```ts
  it('le curseur rend une poignée accessible et valide un pas de clavier', async () => {
    // Un seul composant pour la progression et le volume : ce qui est vérifié
    // ici — la poignée est un `role=slider`, un pas de clavier émet la valeur
    // **et** la valide — est ce que les deux usages supposent.
    const w = mount(Slider, {
      props: { modelValue: [60], min: 0, max: 100, step: 1, 'aria-label': 'Volume' },
      attachTo: document.body,
    })
    await flushPromises()
    const poignee = w.get('[role="slider"]')
    expect(poignee.attributes('aria-valuenow')).toBe('60')
    expect(poignee.attributes('aria-valuemax')).toBe('100')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('update:modelValue')?.[0]).toEqual([[61]])
    expect(w.emitted('valueCommit')?.[0]).toEqual([[61]])
    w.unmount()
  })
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/kit && npx vitest run src/index.test.ts`
Expected : FAIL, `Slider` n'est pas exporté.

- [ ] **Step 3 : le composant**

`web/kit/src/components/ui/slider/index.ts` :

```ts
export { default as Slider } from "./Slider.vue"
```

`web/kit/src/components/ui/slider/Slider.vue` (même patron que `Switch.vue`) :

```vue
<script setup lang="ts">
import type { SliderRootEmits, SliderRootProps } from "reka-ui"
import type { HTMLAttributes } from "vue"
import { reactiveOmit } from "@vueuse/core"
import { SliderRange, SliderRoot, SliderThumb, SliderTrack, useForwardPropsEmits } from "reka-ui"
import { cn } from "@/lib/utils"

// Une seule poignee, toujours : les deux usages du projet (progression, volume)
// sont des valeurs scalaires. `py-[19px]` : 19 + 6 + 19 = 44 px de zone de
// contact autour d'une piste de 6 px — la cible minimale au doigt, portee par
// le padding et non par la piste, qui garde sa finesse.
const props = defineProps<SliderRootProps & { class?: HTMLAttributes["class"] }>()
const emits = defineEmits<SliderRootEmits>()
const delegatedProps = reactiveOmit(props, "class")
const forwarded = useForwardPropsEmits(delegatedProps, emits)
</script>

<template>
  <SliderRoot
    data-slot="slider"
    v-bind="forwarded"
    :class="cn(
      'relative flex w-full touch-none select-none items-center py-[19px] data-[disabled]:opacity-50',
      props.class,
    )"
  >
    <SliderTrack data-slot="slider-track" class="relative h-1.5 w-full grow overflow-hidden rounded-full bg-muted">
      <SliderRange data-slot="slider-range" class="absolute h-full bg-primary" />
    </SliderTrack>
    <SliderThumb
      data-slot="slider-thumb"
      class="block size-4 shrink-0 cursor-pointer rounded-full border border-primary bg-background shadow-sm ring-ring/50 transition-[color,box-shadow] outline-none hover:ring-4 focus-visible:ring-4 disabled:pointer-events-none"
    />
  </SliderRoot>
</template>
```

Dans `web/kit/src/index.ts`, après la ligne `ScrollArea` :

```ts
export { Slider } from './components/ui/slider'
```

- [ ] **Step 4 : vérifier le vert**

Run : `cd web/kit && npx vitest run && npx vue-tsc --noEmit`
Expected : PASS. Si `valueCommit` n'est **pas** émis au pas de clavier sous jsdom (reka le fait au `keydown` via `onSlideEnd`, mais à vérifier), garder l'assertion sur `update:modelValue`, retirer celle sur `valueCommit` de ce test **et le dire dans un commentaire** : la validation au relâchement est alors couverte par le parcours e2e de la Tâche 12.

- [ ] **Step 5 : commit**

```bash
git add web/kit/src/components/ui/slider web/kit/src/index.ts web/kit/src/index.test.ts
git commit -m "feat(kit): composant Slider (reka-ui), zone de contact 44 px"
```

---

### Task 3: `remoteCommands.ts` — le nouveau jeu de commandes

**Files:**
- Modify: `web/app/src/views/remoteCommands.ts`
- Modify: `web/app/src/views/HomeView.test.ts:48-97` (bloc `REMOTE_COMMANDS`), `:589-640` (bloc `indisponible`)
- Modify: `crates/ritornello-core/src/locales/en.toml:36-37,46-47`, `deploy/locales/core/fr.toml` (les quatre mêmes clés)

**Interfaces:**
- Produit :
  ```ts
  export const REMOTE_POWER, REMOTE_SOURCE, REMOTE_MUTE: RemoteCommand
  export const REMOTE_TRANSPORT: RemoteCommand[]            // Prev, PlayPause, Next
  export const REMOTE_TRANSPORT_SECONDAIRE: RemoteCommand[] // Stop, Eject
  export const REMOTE_COMMANDS: RemoteCommand[]             // 8 commandes
  export function indisponible(nom: string, etat: PlayerPayload | null): boolean // veille seulement
  export function masquee(nom: string, etat: PlayerPayload | null): boolean      // Eject sans can_eject
  ```
- `REMOTE_ROWS` **disparaît**, ainsi que `SeekBackward`, `SeekForward`, `VolumeUp`, `VolumeDown` de la page (ils restent dans le protocole et sur la télécommande physique).

- [ ] **Step 1 : les tests qui échouent**

Remplacer le bloc `describe('REMOTE_COMMANDS', …)` de `HomeView.test.ts` (lignes 48-97) par :

```ts
describe('REMOTE_COMMANDS', () => {
  it('couvre les 8 commandes de la page : les ±10 s et le volume pas à pas ont quitté le web', () => {
    // Décidé au chantier refonte : le déplacement passe par la barre, le
    // volume par le curseur. Les quatre commandes retirées restent dans le
    // protocole et sur la télécommande physique.
    expect(REMOTE_COMMANDS).toHaveLength(8)
    expect(REMOTE_COMMANDS.map((c) => c.cmd.cmd).sort()).toEqual(
      ['Eject', 'Mute', 'Next', 'PlayPause', 'Power', 'Prev', 'SourceCycle', 'Stop'].sort(),
    )
  })

  it('le transport va dans le sens du geste, la lecture au centre', () => {
    // |◀ ▶ ▶| : précédent/suivant adjacents à lecture, c'est l'ordre des
    // télécommandes hi-fi ; Stop et Éjecter en retrait.
    expect(REMOTE_TRANSPORT.map((c) => c.cmd.cmd)).toEqual(['Prev', 'PlayPause', 'Next'])
    expect(REMOTE_TRANSPORT_SECONDAIRE.map((c) => c.cmd.cmd)).toEqual(['Stop', 'Eject'])
  })

  it('la veille, la source et le muet sont à part', () => {
    expect(REMOTE_POWER.cmd.cmd).toBe('Power')
    expect(REMOTE_SOURCE.cmd.cmd).toBe('SourceCycle')
    expect(REMOTE_MUTE.cmd.cmd).toBe('Mute')
    const transport = [...REMOTE_TRANSPORT, ...REMOTE_TRANSPORT_SECONDAIRE].map((c) => c.cmd.cmd)
    expect(transport).not.toContain('Power')
    expect(transport).not.toContain('SourceCycle')
    expect(transport).not.toContain('Mute')
  })

  it('chaque commande porte une clé de traduction', () => {
    for (const c of REMOTE_COMMANDS) expect(c.key).toMatch(/^remote_/)
  })
})
```

Remplacer le bloc `describe('indisponible', …)` (lignes 589-640) par :

```ts
describe('indisponible / masquee', () => {
  const etat = (e: Partial<PlayerPayload>): PlayerPayload => ({
    source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
    preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
    duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
    seekable: false, can_eject: false, ...e,
  })

  it('la veille ne laisse passer que Power', () => {
    expect(indisponible('Power', etat({ standby: true }))).toBe(false)
    expect(indisponible('PlayPause', etat({ standby: true }))).toBe(true)
    expect(indisponible('Select', etat({ standby: true }))).toBe(true)
  })

  it('hors veille, rien n’est grisé : le déplacement n’a plus de touche, l’éjection se masque', () => {
    expect(indisponible('PlayPause', etat({}))).toBe(false)
    expect(indisponible('Eject', etat({ can_eject: false }))).toBe(false)
  })

  it('Eject est masqué tant que la source ne déclare pas de tiroir, y compris avant la première trame', () => {
    // `can_eject` est une capacité que le greffon déclare pour lui-même (le cd
    // la déclare disque ou pas) : la masquer ne cache jamais un lecteur qui
    // existe. Avant la première trame, on ne sait pas — donc rien.
    expect(masquee('Eject', null)).toBe(true)
    expect(masquee('Eject', etat({ can_eject: false }))).toBe(true)
    expect(masquee('Eject', etat({ can_eject: true }))).toBe(false)
    expect(masquee('Stop', etat({ can_eject: false }))).toBe(false)
  })

  it('un état inconnu ne grise rien', () => {
    expect(indisponible('PlayPause', null)).toBe(false)
  })
})
```

Mettre à jour l'import en tête du fichier :

```ts
import {
  indisponible, masquee, REMOTE_COMMANDS, REMOTE_MUTE, REMOTE_POWER, REMOTE_SOURCE,
  REMOTE_TRANSPORT, REMOTE_TRANSPORT_SECONDAIRE,
} from './remoteCommands'
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/views/HomeView.test.ts -t "REMOTE_COMMANDS|indisponible"`
Expected : FAIL, `REMOTE_TRANSPORT` / `masquee` non exportés.

- [ ] **Step 3 : réécrire `remoteCommands.ts`**

Remplacer tout ce qui suit `REMOTE_SOURCE` (de la doc de `REMOTE_ROWS` à la fin du fichier) par :

```ts
/**
 * Le muet, a part lui aussi : c'est une bascule, pas un cran sur l'echelle du
 * volume, et il vit sur l'icone du haut-parleur au bout du curseur — la ou on
 * cherche le son.
 */
export const REMOTE_MUTE: RemoteCommand = { key: 'remote_mute', cmd: { cmd: 'Mute' } }

/**
 * Le transport : |◀ ▶ ▶| — precedent et suivant **adjacents** a la lecture,
 * qui est le seul bouton plein. C'est l'ordre des telecommandes hi-fi, de VLC
 * et des lecteurs de bureau : changer de piste est le geste frequent.
 *
 * Plus de « reculer / avancer de 10 s » : decide au chantier refonte, au vu de
 * VLC, Deezer et WMP qui n'en ont pas — c'est la barre d'avancement qui fait
 * ce travail (voir `BarreProgression`). `SeekBackward`/`SeekForward` restent
 * dans le protocole et sur la telecommande physique.
 *
 * Plus de « volume − / + » non plus : le volume est un curseur (`Volume.vue`),
 * pilote au clavier par fleches et Page ↑/↓, ce qui couvre l'accessibilite
 * que les deux touches auraient apportee. Elles restent le geste de la
 * telecommande physique, avec son appui maintenu.
 */
export const REMOTE_TRANSPORT: RemoteCommand[] = [
  { key: 'remote_prev', cmd: { cmd: 'Prev' } },
  { key: 'remote_play_pause', cmd: { cmd: 'PlayPause' } },
  { key: 'remote_next', cmd: { cmd: 'Next' } },
]

/**
 * En retrait du transport : l'arret, et l'ejection quand la source a un tiroir.
 */
export const REMOTE_TRANSPORT_SECONDAIRE: RemoteCommand[] = [
  { key: 'remote_stop', cmd: { cmd: 'Stop' } },
  { key: 'remote_eject', cmd: { cmd: 'Eject' } },
]

/**
 * Toutes les commandes de la page : sert au garde-fou i18n
 * (`i18nKeysUsed.test.ts`) et a verrouiller le compte de huit.
 */
export const REMOTE_COMMANDS: RemoteCommand[] = [
  REMOTE_POWER,
  REMOTE_SOURCE,
  REMOTE_MUTE,
  ...REMOTE_TRANSPORT,
  ...REMOTE_TRANSPORT_SECONDAIRE,
]

/**
 * Une commande que l'appareil ignorerait dans l'état courant : son bouton est
 * grisé plutôt qu'offert.
 *
 * Une seule règle désormais : en **veille**, le cœur retourne sans rien faire
 * sur tout ce qui n'est pas `Power` (première ligne de `handle_command`),
 * grille des présélections comprise. Le déplacement n'a plus de touche (c'est
 * la barre qui se rend inerte, sur `seekable`), et l'éjection se **masque**
 * plutôt que de se griser — voir `masquee`.
 *
 * Un état pas encore reçu (`null`) ne grise rien : la télécommande s'ouvre
 * utilisable, et la trame corrige aussitôt.
 */
export function indisponible(nom: string, etat: PlayerPayload | null): boolean {
  if (!etat) return false
  return etat.standby && nom !== 'Power'
}

/**
 * Une commande qui n'a pas lieu d'être sur cette source : son bouton n'est pas
 * rendu du tout.
 *
 * Seul `Eject` est concerné. `can_eject` est une capacité que le greffon source
 * déclare **pour lui-même** (`SourcePlugin::can_eject` du sdk) : le lecteur de
 * cd la déclare qu'il y ait un disque ou non, la radio ne la déclare pas.
 * Masquer sur cette base ne cache donc jamais un lecteur qui existe — au
 * contraire d'un grisage, qui affirmait une touche là où il n'y a pas de
 * tiroir. Avant la première trame on ne sait pas : rien n'est rendu, et la
 * trame corrige.
 */
export function masquee(nom: string, etat: PlayerPayload | null): boolean {
  return nom === 'Eject' && !(etat?.can_eject ?? false)
}
```

Mettre à jour le commentaire d'en-tête du fichier (« Douze commandes simples ») : remplacer par « Huit commandes sur la page, sur les douze du protocole : les ±10 s et le volume pas à pas n'ont plus de touche web (voir `REMOTE_TRANSPORT`). »

- [ ] **Step 4 : retirer les quatre clés i18n**

`crates/ritornello-core/src/locales/en.toml` : supprimer les lignes `remote_vol_up`, `remote_vol_down`, `remote_seek_back`, `remote_seek_forward`. `deploy/locales/core/fr.toml` : supprimer les quatre mêmes clés. Vérifier qu'aucun Rust ne les lit : `grep -rn "remote_vol_up\|remote_seek" crates --include=*.rs` doit être vide (vérifié le 2026-08-26 : vide).

- [ ] **Step 5 : vérifier**

Run : `cd web/app && npx vitest run src/views/HomeView.test.ts -t "REMOTE_COMMANDS|indisponible" && npx vitest run src/i18nKeysUsed.test.ts`
Expected : PASS pour ces blocs. **État transitoire assumé** : `HomeView.vue` importe encore `REMOTE_ROWS` et ne compile plus (`vue-tsc`) jusqu'à la Tâche 10 ; les autres blocs de `HomeView.test.ts` sont rouges d'ici là. C'est accepté parce que les commits seront squashés par grande fonction à la fusion (Tâche 14) ; ne pas « réparer » `HomeView.vue` à la marge ici, il est réécrit en entier à la Tâche 10. Run (WSL) : `cargo test -p ritornello-core parite` → PASS.

- [ ] **Step 6 : commit**

```bash
git add web/app/src/views/remoteCommands.ts web/app/src/views/HomeView.test.ts crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "refactor(web): huit commandes sur la page, Eject masque sans tiroir"
```

---

### Task 4: Types et composable `usePresets`

**Files:**
- Modify: `web/app/src/types.ts`
- Create: `web/app/src/composables/usePresets.ts`
- Test: `web/app/src/composables/usePresets.test.ts`

**Interfaces:**
- Produit dans `types.ts` :
  ```ts
  export type Playback = 'playing' | 'paused'
  // PlayerPayload gagne : playback?: Playback   (absent = arrêté, idiome de `seekable`)
  export interface PresetNomme { index: number; name: string }
  export interface SourcePresets { name: string; presets?: PresetNomme[] }
  export interface PresetsPayload { sources: SourcePresets[] }
  ```
- Produit dans `usePresets.ts` : `usePresets(): { recharger(): Promise<void>; nomDe(source: string, n: number): string | null }`.

- [ ] **Step 1 : le test qui échoue**

`web/app/src/composables/usePresets.test.ts` :

```ts
import { afterEach, describe, expect, it, vi } from 'vitest'
import { usePresets } from './usePresets'

describe('usePresets', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('nomme une présélection par source et numéro', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      sources: [
        { name: 'radio', presets: [{ index: 1, name: 'FIP' }, { index: 2, name: 'France Inter' }] },
        { name: 'cd' },
      ],
    }), { status: 200 })))
    const { recharger, nomDe } = usePresets()
    expect(nomDe('radio', 1)).toBeNull()
    await recharger()
    expect(nomDe('radio', 1)).toBe('FIP')
    expect(nomDe('radio', 2)).toBe('France Inter')
    // Une source sans liste (le cd) : numéros seuls, comme aujourd'hui.
    expect(nomDe('cd', 1)).toBeNull()
    expect(nomDe('radio', 9)).toBeNull()
  })

  it('un cœur injoignable laisse la liste précédente', async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response(JSON.stringify({ sources: [{ name: 'radio', presets: [{ index: 1, name: 'FIP' }] }] }), { status: 200 }))
      .mockRejectedValueOnce(new Error('réseau'))
    vi.stubGlobal('fetch', fetch)
    const { recharger, nomDe } = usePresets()
    await recharger()
    await recharger()
    expect(nomDe('radio', 1)).toBe('FIP')
  })
})
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/composables/usePresets.test.ts`
Expected : FAIL, module introuvable.

- [ ] **Step 3 : types et composable**

Dans `types.ts`, après `export type Command = …` :

```ts
/** Ce que fait le lecteur. Absent de la trame quand il est arrêté (idiome de `seekable`). */
export type Playback = 'playing' | 'paused'
/** Une présélection nommée telle que `GET /api/presets` la sert. */
export interface PresetNomme { index: number; name: string }
/** Une source et sa liste ; `presets` est absent quand elle n'énumère pas. */
export interface SourcePresets { name: string; presets?: PresetNomme[] }
/** Le catalogue des sources, tel que le cœur le diffuse aux afficheurs. */
export interface PresetsPayload { sources: SourcePresets[] }
```

Dans `PlayerPayload`, après `can_eject: boolean` :

```ts
  /**
   * Ce que fait le lecteur : `playing`, `paused`, ou absent quand rien ne joue.
   * C'est ce qui choisit l'icône du bouton de lecture (▶ ou ❚❚). Le champ
   * voyageait déjà sans être lu.
   */
  playback?: Playback
```

`web/app/src/composables/usePresets.ts` :

```ts
import { api } from '@ritornello/ui'
import { ref } from 'vue'
import type { PresetsPayload } from '../types'

/**
 * Les noms des présélections, par source puis par numéro, lus sur
 * `GET /api/presets` — le catalogue que le cœur tient déjà pour les afficheurs.
 *
 * Local à l'appelant (pas d'état de module) : seule la page d'accueil s'en
 * sert, et elle recharge quand la source active change (voir `HomeView`). Un
 * échec conserve la liste précédente : une coupure passagère ne doit pas
 * dénommer les tuiles.
 */
export function usePresets() {
  const noms = ref<Map<string, Map<number, string>>>(new Map())

  async function recharger(): Promise<void> {
    const charge = await api.get<PresetsPayload>('/api/presets').catch((e: unknown) => {
      console.warn('GET /api/presets indisponible : tuiles sans nom', e)
      return null
    })
    if (!charge) return
    noms.value = new Map(
      charge.sources.map((s) => [s.name, new Map((s.presets ?? []).map((p) => [p.index, p.name]))]),
    )
  }

  function nomDe(source: string, n: number): string | null {
    return noms.value.get(source)?.get(n) ?? null
  }

  return { recharger, nomDe }
}
```

- [ ] **Step 4 : vérifier**

Run : `cd web/app && npx vitest run src/composables/usePresets.test.ts`
Expected : PASS.

- [ ] **Step 5 : commit**

```bash
git add web/app/src/types.ts web/app/src/composables/usePresets.ts web/app/src/composables/usePresets.test.ts
git commit -m "feat(web): usePresets lit /api/presets, type playback"
```

---

### Task 5: `BarreProgression` devient un curseur tactile (trois états)

**Files:**
- Modify: `web/app/src/components/BarreProgression.vue`
- Modify: `web/app/src/components/BarreProgression.test.ts`

**Interfaces:**
- Consomme : `Slider` du kit (Tâche 2).
- Produit : mêmes props (`position`, `duree`, `deplacable`, `pas`) et même émission `deplacer: [secondes: number]`. Trois états rendus :
  1. `position === null` → rien (`data-progression` absent) ;
  2. position connue, `deplacable === false` → barre statique `data-barre` sans `role`, sans poignée, remplissage `data-remplissage` ;
  3. `deplacable === true` → `Slider` (`data-barre`, poignée `role=slider`), glisser local, **un** `deplacer` au relâchement, valeur visée tenue jusqu'à la trame qui la rejoint.

- [ ] **Step 1 : les tests qui échouent**

Remplacer le contenu de `BarreProgression.test.ts` à partir du test `'emet la seconde visee au clic'` (garder les quatre premiers tests tels quels) par :

```ts
  // reka-ui capture le pointeur pendant le glisser ; jsdom n'implemente pas
  // cette API. Trois cales, le temps du fichier.
  beforeAll(() => {
    Element.prototype.setPointerCapture ??= () => {}
    Element.prototype.releasePointerCapture ??= () => {}
    Element.prototype.hasPointerCapture ??= () => true
  })

  function rectangle(w: ReturnType<typeof monte>) {
    const piste = w.get('[data-slot="slider"]')
    piste.element.getBoundingClientRect = () =>
      ({ left: 0, width: 200, top: 0, height: 44, right: 200, bottom: 44, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect
    return piste
  }

  it('un contenu deplacable rend une poignee accessible', () => {
    const w = monte({ deplacable: true })
    const poignee = w.get('[role="slider"]')
    expect(poignee.attributes('aria-valuenow')).toBe('87')
    expect(poignee.attributes('aria-valuemax')).toBe('254')
  })

  it('le glisser suit le doigt localement et ne valide qu au relachement', async () => {
    // Un seul `SeekTo` par geste : pendant le glisser, seul l'affichage bouge.
    const w = monte({ deplacable: true })
    const piste = rectangle(w)
    await piste.trigger('pointerdown', { clientX: 100, pointerId: 1, button: 0 })
    await piste.trigger('pointermove', { clientX: 150, pointerId: 1 })
    expect(w.emitted('deplacer')).toBeUndefined()
    expect(w.get('[data-position]').text()).toBe('3:10') // 150/200 × 254 = 190 s, affiché pendant le geste
    await piste.trigger('pointerup', { clientX: 150, pointerId: 1 })
    expect(w.emitted('deplacer')).toEqual([[190]])
  })

  it('la valeur visee tient jusqu a la trame qui la rejoint', async () => {
    // Sans cela, la trame suivante (position d'avant le saut) ramenait la
    // poignée en arrière un instant — le défaut visible des lecteurs naïfs.
    const w = monte({ deplacable: true })
    const piste = rectangle(w)
    await piste.trigger('pointerdown', { clientX: 100, pointerId: 1, button: 0 })
    await piste.trigger('pointerup', { clientX: 100, pointerId: 1 })
    expect(w.emitted('deplacer')).toEqual([[127]])
    await w.setProps({ position: 88 }) // la trame d'avant le saut
    expect(w.get('[data-position]').text()).toBe('2:07')
    await w.setProps({ position: 129 }) // à un pas près : on la rejoint
    expect(w.get('[data-position]').text()).toBe('2:09')
  })

  // Sans le clavier, la barre serait la seule commande de la page hors
  // d'atteinte sans souris. Le pas est celui des touches physiques
  // (`seek_step_s`), pas la seconde du curseur.
  it('le clavier deplace du pas configure, borne aux deux bouts', async () => {
    const w = monte({ deplacable: true, position: 250 })
    const poignee = w.get('[role="slider"]')
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('deplacer')?.[0]).toEqual([254])
    await poignee.trigger('keydown', { key: 'Home' })
    expect(w.emitted('deplacer')?.[1]).toEqual([0])
    await poignee.trigger('keydown', { key: 'ArrowLeft' })
    expect(w.emitted('deplacer')?.[2]).toEqual([240])
  })
})
```

Ajouter `beforeAll` à l'import de vitest. Si le fichier avait déjà des tests clavier après `'emet la seconde visee au clic'`, ils sont remplacés par celui ci-dessus.

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/components/BarreProgression.test.ts`
Expected : FAIL (`[data-slot="slider"]` introuvable).

- [ ] **Step 3 : le composant**

Réécrire `BarreProgression.vue` :

```vue
<script setup lang="ts">
import { Slider } from '@ritornello/ui'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import { formateDuree, formatePosition } from '../composables/usePlayer'

// Trois etats, et la charge utile les distingue tous :
//  - position inconnue : rien (une radio sans greffon de metadonnees) ;
//  - position connue mais pas `seekable` : une barre qui informe, sans
//    poignee ni role (Radio France annonce la duree d'un direct qu'on ne
//    rembobine pas) ;
//  - `seekable` : un vrai curseur, au doigt comme au clavier.
const { t } = useCatalog()
const props = defineProps<{
  position: number | null
  duree: number | null
  /** Le contenu accepte un deplacement (`seekable` de la charge utile). */
  deplacable: boolean
  /** Pas du clavier, en secondes : le meme que celui des touches physiques. */
  pas: number
}>()
const emit = defineEmits<{ deplacer: [secondes: number] }>()

// Valeur sous le doigt pendant le glisser ; null hors geste.
const locale = ref<number | null>(null)
// Valeur validee, en attente de la trame qui la confirme. Sans elle, la trame
// suivante — celle d'avant le saut, deja en route — ramenait la poignee en
// arriere un instant.
const visee = ref<number | null>(null)
watch(
  () => props.position,
  (p) => {
    if (visee.value !== null && p != null && Math.abs(p - visee.value) <= props.pas) visee.value = null
  },
)
const affichee = computed(() => locale.value ?? visee.value ?? props.position)

const texteEcoule = computed(() => formatePosition(affichee.value))
// formateDuree, pas formatePosition : ce dernier accepte zero, alors qu'une
// duree totale de "0:00" n'en est pas une (voir sa doc dans usePlayer.ts).
const texteDuree = computed(() => formateDuree(props.duree))
// Une barre sans fin n'apprend rien : sans duree connue, seul l'ecoule s'affiche.
const barreVisible = computed(() => props.duree != null && props.duree > 0)
const pourcent = computed(() => {
  if (!barreVisible.value || affichee.value == null) return 0
  return Math.min(100, Math.max(0, (affichee.value / (props.duree as number)) * 100))
})

function surChangement(v: number[]): void {
  locale.value = v[0] ?? 0
}

function surValidation(v: number[]): void {
  const s = Math.round(v[0] ?? 0)
  locale.value = null
  visee.value = s
  emit('deplacer', s)
}

// Le clavier reste au pas des touches physiques, pas a la seconde du curseur :
// capture sur l'enveloppe, avant le gestionnaire de reka sur la poignee.
function auClavier(e: KeyboardEvent): void {
  if (!props.deplacable || props.duree == null) return
  const depuis = affichee.value ?? 0
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
  e.stopPropagation()
  surValidation([Math.min(props.duree, Math.max(0, cible))])
}
</script>

<template>
  <div v-if="texteEcoule" class="space-y-1" data-progression>
    <div v-if="barreVisible && deplacable" @keydown.capture="auClavier">
      <Slider
        data-barre
        :model-value="[affichee ?? 0]"
        :min="0"
        :max="duree ?? 0"
        :step="1"
        :aria-label="t('position_label')"
        @update:model-value="surChangement"
        @value-commit="surValidation"
      />
    </div>
    <div v-else-if="barreVisible" class="py-[19px]" data-barre>
      <div class="h-1.5 w-full overflow-hidden rounded-full bg-muted">
        <div class="h-full rounded-full bg-primary" :style="{ width: pourcent + '%' }" data-remplissage />
      </div>
    </div>
    <div class="flex justify-between text-xs text-muted-foreground">
      <span data-position>{{ texteEcoule }}</span>
      <span v-if="texteDuree" data-duree-totale>{{ texteDuree }}</span>
    </div>
  </div>
</template>
```

Le test existant `'remplit la barre au prorata'` lit `[data-remplissage]` : il reste vrai dans l'état statique (props par défaut `deplacable: false`). Le test `'inerte quand le contenu n est pas deplacable'` reste vrai (`data-barre` statique, pas de `role`).

- [ ] **Step 4 : vérifier**

Run : `cd web/app && npx vitest run src/components/BarreProgression.test.ts`
Expected : PASS. Si le glisser au pointeur ne déclenche pas `valueCommit` sous jsdom malgré les cales (reka lit `event.button`, `pointerId`, `getBoundingClientRect` de **la piste** — vérifier que la cale de rectangle est posée sur `[data-slot="slider"]`), remplacer `pointerdown/move/up` par l'appel direct des gestionnaires via `w.getComponent(Slider).vm.$emit('update:modelValue', [190])` puis `$emit('valueCommit', [190])`, et le dire en commentaire : le geste réel est alors couvert par l'e2e (Tâche 12).

- [ ] **Step 5 : commit**

```bash
git add web/app/src/components/BarreProgression.vue web/app/src/components/BarreProgression.test.ts
git commit -m "feat(web): barre de progression au doigt, un seul SeekTo au relachement"
```

---

### Task 6: `Volume.vue` — curseur et Muet

**Files:**
- Create: `web/app/src/components/Volume.vue`
- Test: `web/app/src/components/Volume.test.ts`

**Interfaces:**
- Produit : `<Volume :volume="number | null" :muted="boolean" :desactive="boolean" @regler="(v: number) => …" @muet="() => …" />`. Marqueurs : `data-volume` (texte `"60 %"`), `data-volume-curseur`, `data-remote-command="Mute"` sur l'icône (avec `aria-pressed`, `data-actif` quand muet), `data-muted` badge conservé.

- [ ] **Step 1 : les tests qui échouent**

```ts
import { mount } from '@vue/test-utils'
import { beforeAll, describe, expect, it, vi } from 'vitest'
import Volume from './Volume.vue'

const monte = (props: Record<string, unknown> = {}) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Volume, { props: { volume: 60, muted: false, desactive: false, ...props } })
}

describe('Volume', () => {
  beforeAll(() => {
    Element.prototype.setPointerCapture ??= () => {}
    Element.prototype.releasePointerCapture ??= () => {}
    Element.prototype.hasPointerCapture ??= () => true
  })

  it('affiche la valeur et une poignée à cette valeur', () => {
    const w = monte()
    expect(w.get('[data-volume]').text()).toBe('60 %')
    expect(w.get('[role="slider"]').attributes('aria-valuenow')).toBe('60')
  })

  it('n’affiche rien avant la première trame', () => {
    const w = monte({ volume: null })
    expect(w.get('[data-volume]').text()).toBe('')
  })

  it('valide un réglage absolu au relâchement, une seule fois', async () => {
    const w = monte()
    const poignee = w.get('[role="slider"]')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(w.emitted('regler')).toEqual([[61]])
    expect(w.get('[data-volume]').text()).toBe('61 %')
  })

  it('le haut-parleur est la bascule Muet, et dit son état', async () => {
    // Demandé à l'usage sur l'ancienne page : on lisait « Volume : 60 % » sans
    // comprendre pourquoi rien ne sortait. Ici le muet barre la valeur et
    // change l'icône, au seul endroit où l'on cherche le son.
    const w = monte({ muted: true })
    const bouton = w.get('[data-remote-command="Mute"]')
    expect(bouton.attributes('aria-pressed')).toBe('true')
    expect(bouton.attributes('data-actif')).toBe('true')
    expect(w.get('[data-volume]').classes()).toContain('line-through')
    await bouton.trigger('click')
    expect(w.emitted('muet')).toHaveLength(1)
  })

  it('en veille, curseur et bascule sont grisés', () => {
    const w = monte({ desactive: true })
    expect(w.get('[data-remote-command="Mute"]').attributes('disabled')).toBeDefined()
    expect(w.get('[data-slot="slider"]').attributes('data-disabled')).toBeDefined()
  })
})
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/components/Volume.test.ts`
Expected : FAIL, module introuvable.

- [ ] **Step 3 : le composant**

```vue
<script setup lang="ts">
import { Badge, Button, Slider } from '@ritornello/ui'
import { SpeakerLoudIcon, SpeakerOffIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'

// Le volume est un reglage continu : un curseur, pas deux touches. Le clavier
// (fleches = 1 %, Page = 10 %, Debut/Fin) et le `role=slider` de reka couvrent
// l'accessibilite ; les touches − / + restent celles de la telecommande
// physique. La commande envoyee est `SetVolume` (absolue), une seule au
// relachement — pendant le geste, seul l'affichage bouge.
const { t } = useCatalog()
const props = defineProps<{ volume: number | null; muted: boolean; desactive: boolean }>()
const emit = defineEmits<{ regler: [pourcent: number]; muet: [] }>()

const locale = ref<number | null>(null)
// Meme raison que dans BarreProgression : la trame d'avant le reglage ne doit
// pas faire reculer la poignee un instant.
const visee = ref<number | null>(null)
watch(
  () => props.volume,
  (v) => {
    if (visee.value !== null && v === visee.value) visee.value = null
  },
)
const affiche = computed(() => locale.value ?? visee.value ?? props.volume)

function surChangement(v: number[]): void {
  locale.value = v[0] ?? 0
}
function surValidation(v: number[]): void {
  const p = Math.round(v[0] ?? 0)
  locale.value = null
  visee.value = p
  emit('regler', p)
}
</script>

<template>
  <div class="flex items-center gap-3" data-volume-ligne>
    <!-- L'icône **est** la bascule : c'est là qu'on cherche le son. -->
    <Button
      variant="ghost"
      size="icon"
      data-remote-command="Mute"
      :data-actif="muted ? 'true' : undefined"
      :aria-pressed="String(muted)"
      :aria-label="t('remote_mute')"
      :disabled="desactive"
      @click="emit('muet')"
    >
      <SpeakerOffIcon v-if="muted" class="size-5" />
      <SpeakerLoudIcon v-else class="size-5" />
    </Button>
    <Slider
      class="flex-1"
      data-volume-curseur
      :model-value="[affiche ?? 0]"
      :min="0"
      :max="100"
      :step="1"
      :disabled="desactive || affiche === null"
      :aria-label="t('volume')"
      @update:model-value="surChangement"
      @value-commit="surValidation"
    />
    <span
      class="w-12 text-right text-sm tabular-nums text-foreground"
      :class="{ 'line-through opacity-60': muted }"
      data-volume
      >{{ affiche === null ? '' : affiche + ' %' }}</span
    >
    <Badge v-if="muted" variant="secondary" data-muted>{{ t('muted') }}</Badge>
  </div>
</template>
```

- [ ] **Step 4 : vérifier**

Run : `cd web/app && npx vitest run src/components/Volume.test.ts`
Expected : PASS.

- [ ] **Step 5 : commit**

```bash
git add web/app/src/components/Volume.vue web/app/src/components/Volume.test.ts
git commit -m "feat(web): curseur de volume, le haut-parleur est la bascule Muet"
```

---

### Task 7: `Transport.vue` et les deux icônes maison

**Files:**
- Create: `web/app/src/components/icones/IconeVeille.vue`, `web/app/src/components/icones/IconeEjecter.vue`
- Create: `web/app/src/components/Transport.vue`
- Test: `web/app/src/components/Transport.test.ts`

**Interfaces:**
- Consomme : `REMOTE_TRANSPORT`, `REMOTE_TRANSPORT_SECONDAIRE`, `indisponible`, `masquee` (Tâche 3) ; `PlayerPayload.playback` (Tâche 4).
- Produit : `<Transport :etat="PlayerPayload | null" @commande="(c: Command) => …" />`. Chaque bouton porte `data-remote-command="<cmd>"` et un `aria-label` traduit ; le bouton de lecture porte `data-playback="playing|paused|stopped"`.

- [ ] **Step 1 : les tests qui échouent**

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import Transport from './Transport.vue'

const etat = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, ...e,
})
const monte = (e: Partial<PlayerPayload> | null) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(Transport, { props: { etat: e ? etat(e) : null } })
}

describe('Transport', () => {
  it('rend |◀ ▶ ▶| ■ dans cet ordre, sans Éjecter sur une source sans tiroir', () => {
    const w = monte({})
    expect(w.findAll('[data-remote-command]').map((b) => b.attributes('data-remote-command')))
      .toEqual(['Prev', 'PlayPause', 'Next', 'Stop'])
  })

  it('Éjecter apparaît quand la source déclare un tiroir', () => {
    const w = monte({ can_eject: true })
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })

  it('l’icône de lecture suit playback', async () => {
    const w = monte({ playback: 'playing' })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('playing')
    await w.setProps({ etat: etat({}) })
    expect(w.get('[data-remote-command="PlayPause"]').attributes('data-playback')).toBe('stopped')
  })

  it('poste la commande du bouton', async () => {
    const w = monte({})
    await w.get('[data-remote-command="Next"]').trigger('click')
    expect(w.emitted('commande')).toEqual([[{ cmd: 'Next' }]])
  })

  it('en veille tout est grisé', () => {
    const w = monte({ standby: true })
    for (const b of w.findAll('[data-remote-command]')) expect(b.attributes('disabled')).toBeDefined()
  })
})
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/components/Transport.test.ts` → FAIL, module introuvable.

- [ ] **Step 3 : les icônes que Radix n'a pas**

`IconeEjecter.vue` (grille 15×15, trait `currentColor`, comme `@radix-icons/vue`) :

```vue
<template>
  <svg width="15" height="15" viewBox="0 0 15 15" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <path d="M7.5 3L12.5 9H2.5L7.5 3Z" stroke="currentColor" stroke-linejoin="round" />
    <path d="M2.5 11.5H12.5" stroke="currentColor" stroke-linecap="round" />
  </svg>
</template>
```

`IconeVeille.vue` :

```vue
<template>
  <svg width="15" height="15" viewBox="0 0 15 15" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <path d="M4.7 4.3A4.5 4.5 0 1 0 10.3 4.3" stroke="currentColor" stroke-linecap="round" />
    <path d="M7.5 1.5V7.5" stroke="currentColor" stroke-linecap="round" />
  </svg>
</template>
```

- [ ] **Step 4 : le composant**

```vue
<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { PauseIcon, PlayIcon, StopIcon, TrackNextIcon, TrackPreviousIcon } from '@radix-icons/vue'
import type { Component } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { Command, PlayerPayload } from '../types'
import {
  indisponible, masquee, REMOTE_TRANSPORT, REMOTE_TRANSPORT_SECONDAIRE,
} from '../views/remoteCommands'
import type { RemoteCommand } from '../views/remoteCommands'
import IconeEjecter from './icones/IconeEjecter.vue'

// Le transport en icones, la lecture seule en plein : c'est le geste frequent,
// et l'oeil la trouve sans lire. L'ordre est celui de `REMOTE_TRANSPORT`.
const { t } = useCatalog()
const props = defineProps<{ etat: PlayerPayload | null }>()
const emit = defineEmits<{ commande: [cmd: Command] }>()

const ICONES: Record<string, Component> = {
  Prev: TrackPreviousIcon,
  Next: TrackNextIcon,
  Stop: StopIcon,
  Eject: IconeEjecter,
}

function icone(c: RemoteCommand): Component {
  if (c.cmd.cmd === 'PlayPause') return props.etat?.playback === 'playing' ? PauseIcon : PlayIcon
  return ICONES[c.cmd.cmd] ?? PlayIcon
}

const visibles = (liste: RemoteCommand[]) => liste.filter((c) => !masquee(c.cmd.cmd, props.etat))
</script>

<template>
  <div class="flex items-center justify-center gap-3 md:justify-start md:gap-2" data-transport>
    <Button
      v-for="c in visibles(REMOTE_TRANSPORT)"
      :key="c.key"
      :data-remote-command="c.cmd.cmd"
      :data-playback="c.cmd.cmd === 'PlayPause' ? (etat?.playback ?? 'stopped') : undefined"
      :variant="c.cmd.cmd === 'PlayPause' ? 'default' : 'ghost'"
      :class="c.cmd.cmd === 'PlayPause' ? 'size-16 rounded-full md:size-12' : 'size-12 rounded-full md:size-10'"
      :aria-label="t(c.key)"
      :title="t(c.key)"
      :disabled="indisponible(c.cmd.cmd, etat)"
      @click="emit('commande', c.cmd)"
    >
      <component :is="icone(c)" :class="c.cmd.cmd === 'PlayPause' ? 'size-7 md:size-6' : 'size-6 md:size-5'" />
    </Button>
    <!-- En retrait : a droite sur PC, en fin de rangee sur telephone. -->
    <div class="flex items-center gap-1 md:ml-auto">
      <Button
        v-for="c in visibles(REMOTE_TRANSPORT_SECONDAIRE)"
        :key="c.key"
        :data-remote-command="c.cmd.cmd"
        variant="ghost"
        class="size-12 rounded-full text-muted-foreground md:size-10"
        :aria-label="t(c.key)"
        :title="t(c.key)"
        :disabled="indisponible(c.cmd.cmd, etat)"
        @click="emit('commande', c.cmd)"
      >
        <component :is="icone(c)" class="size-5" />
      </Button>
    </div>
  </div>
</template>
```

- [ ] **Step 5 : vérifier**

Run : `cd web/app && npx vitest run src/components/Transport.test.ts` → PASS.

- [ ] **Step 6 : commit**

```bash
git add web/app/src/components/Transport.vue web/app/src/components/Transport.test.ts web/app/src/components/icones
git commit -m "feat(web): transport en icones, lecture dominante, Eject masque sans tiroir"
```

---

### Task 8: `GrillePresets.vue` — tuiles nommées, logique extraite de `HomeView`

**Files:**
- Create: `web/app/src/components/GrillePresets.vue`
- Modify: `web/app/src/views/HomeView.test.ts` — les blocs `'HomeView — pagination des présélections'` et `'HomeView — la page suit ce qui joue'` restent sur `HomeView` (intégration) et doivent passer **après la Tâche 9** ; cette tâche ajoute un test unitaire ciblé.
- Test: `web/app/src/components/GrillePresets.test.ts`

**Interfaces:**
- Consomme : `PlayerPayload` ; `nomDe` de `usePresets` (Tâche 4).
- Produit : `<GrillePresets :etat="PlayerPayload | null" :nom-de="(n: number) => string | null" @choisir="(n: number) => …" />`. Marqueurs conservés : `data-preset-button="n"`, `data-preset-active`, `aria-current`, `data-preset-prev`, `data-preset-next`, `data-preset-count`. Nouveau : `data-preset-name` sur le nom dans la tuile.
- La logique de page (`page`, `compte`, `presets`, `paginationVisible`, `dernierePage`, `pageDe`, le `watch([compte, presetActif])`) est **déplacée** telle quelle depuis `HomeView.vue` — mêmes bornes que le `+10` du cœur, ne pas la réécrire.

- [ ] **Step 1 : le test qui échoue**

```ts
import { mount } from '@vue/test-utils'
import { describe, expect, it, vi } from 'vitest'
import type { PlayerPayload } from '../types'
import GrillePresets from './GrillePresets.vue'

const etat = (e: Partial<PlayerPayload>): PlayerPayload => ({
  source: 'radio', volume: 60, muted: false, standby: false, preset: null, preset_count: null,
  preset_name: null, status: null, overlay: null, artist: null, title: null, album: null,
  duration_s: null, origin: null, cover_href: null, cover_origin: null, position_s: null,
  seekable: false, can_eject: false, ...e,
})
const NOMS: Record<number, string> = { 1: 'FIP', 2: 'France Inter' }
const monte = (e: Partial<PlayerPayload>, nomDe = (n: number) => NOMS[n] ?? null) => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
  return mount(GrillePresets, { props: { etat: etat(e), nomDe } })
}

describe('GrillePresets', () => {
  it('nomme les tuiles que la source nomme, numéro seul sinon', () => {
    const w = monte({ preset_count: 3, preset: 1 })
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    expect(w.get('[data-preset-button="2"] [data-preset-name]').text()).toBe('France Inter')
    expect(w.find('[data-preset-button="3"] [data-preset-name]').exists()).toBe(false)
    expect(w.get('[data-preset-button="3"]').text()).toBe('3')
  })

  it('met en évidence la tuile qui joue', () => {
    const w = monte({ preset_count: 3, preset: 2 })
    expect(w.get('[data-preset-button="2"]').attributes('aria-current')).toBe('true')
    expect(w.findAll('[data-preset-active]')).toHaveLength(1)
  })

  it('émet le numéro choisi', async () => {
    const w = monte({ preset_count: 3 })
    await w.get('[data-preset-button="3"]').trigger('click')
    expect(w.emitted('choisir')).toEqual([[3]])
  })

  it('annonce le compte et la fenêtre affichée', () => {
    const w = monte({ preset_count: 12, preset: 11 })
    expect(w.get('[data-preset-count]').text()).toContain('12')
    expect(w.get('[data-preset-fenetre]').text()).toBe('10–12')
    expect(w.get('[data-preset-prev]').attributes('disabled')).toBeUndefined()
    expect(w.get('[data-preset-next]').attributes('disabled')).toBeDefined()
  })

  it('sans compte déclaré, 1-9 nus et pas de flèches', () => {
    const w = monte({}, () => null)
    expect(w.findAll('[data-preset-button]')).toHaveLength(9)
    expect(w.find('[data-preset-prev]').exists()).toBe(false)
    expect(w.find('[data-preset-count]').exists()).toBe(false)
  })
})
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/components/GrillePresets.test.ts` → FAIL.

- [ ] **Step 3 : le composant**

```vue
<script setup lang="ts">
import { Button } from '@ritornello/ui'
import { ChevronLeftIcon, ChevronRightIcon } from '@radix-icons/vue'
import { computed, ref, watch } from 'vue'
import { useCatalog } from '../composables/useCatalog'
import type { PlayerPayload } from '../types'
import { indisponible } from '../views/remoteCommands'

const { t } = useCatalog()
const props = defineProps<{ etat: PlayerPayload | null; nomDe: (n: number) => string | null }>()
const emit = defineEmits<{ choisir: [n: number] }>()

const page = ref(0)

// Compte déclaré par la source (null = source muette sur le sujet : grille
// 1-9 historique, pour ne jamais désarmer la télécommande).
const compte = computed(() => props.etat?.preset_count ?? null)

// Numéros de la page courante, seulement ceux qui existent. Page 0 : 1-9 (les
// touches nues de la télécommande) ; page k : 10k à 10k+9 (le 0 de la
// télécommande donne 10k). Mêmes bornes que le `+10` du cœur — ne pas
// « simplifier » en fenêtres de 6 ou 10 : la page web et la touche physique
// doivent désigner les mêmes groupes.
const presets = computed(() => {
  const c = compte.value
  if (c === null) return Array.from({ length: 9 }, (_, i) => i + 1)
  const debut = page.value === 0 ? 1 : page.value * 10
  const fin = Math.min(page.value * 10 + 9, c)
  return debut > fin ? [] : Array.from({ length: fin - debut + 1 }, (_, i) => debut + i)
})

const paginationVisible = computed(() => (compte.value ?? 0) > 9)

// Dernière page non vide : le plus grand multiple de 10 encore atteignable
// (même borne que le rebouclage du +10 côté cœur), 0 si tout tient sur 1-9.
const dernierePage = computed(() => {
  const c = compte.value ?? 0
  return c > 9 ? Math.floor(c / 10) : 0
})

const fenetre = computed(() => {
  const p = presets.value
  return p.length ? `${p[0]}–${p[p.length - 1]}` : ''
})

function pagePrecedente() {
  if (page.value > 0) page.value -= 1
}
function pageSuivante() {
  if (page.value < dernierePage.value) page.value += 1
}

const presetActif = computed(() => props.etat?.preset ?? null)

function pageDe(n: number) {
  return n < 10 ? 0 : Math.floor(n / 10)
}

// La page suit ce qui joue (télécommande infrarouge, +10, piste suivante) ;
// faute de présélection déclarée, un changement de compte ramène en première
// page. Un seul observateur pour les deux champs : ils arrivent dans la même
// trame. (Déplacé tel quel depuis HomeView.)
watch([compte, presetActif], (_, [compteAvant]) => {
  if (presetActif.value !== null) {
    page.value = Math.min(pageDe(presetActif.value), dernierePage.value)
    return
  }
  if (compte.value !== compteAvant) page.value = 0
})

const grisees = computed(() => indisponible('Select', props.etat))
</script>

<template>
  <div class="space-y-3" data-grille-presets>
    <div v-if="compte !== null" class="flex items-center gap-2">
      <p data-preset-count class="text-xs text-muted-foreground">{{ t('presets_label') }} : {{ compte }}</p>
      <span class="flex-1" />
      <template v-if="paginationVisible">
        <Button data-preset-prev variant="outline" size="icon-sm" :disabled="page === 0" :aria-label="t('presets_prev_page')" @click="pagePrecedente">
          <ChevronLeftIcon class="size-4" />
        </Button>
        <span class="text-xs tabular-nums text-muted-foreground" data-preset-fenetre>{{ fenetre }}</span>
        <Button data-preset-next variant="outline" size="icon-sm" :disabled="page === dernierePage" :aria-label="t('presets_next_page')" @click="pageSuivante">
          <ChevronRightIcon class="size-4" />
        </Button>
      </template>
    </div>
    <!-- Une tuile = numero + nom. Deux colonnes : assez pour un nom de station,
         et la meme grille sur telephone et dans la demi-largeur du PC. -->
    <div class="grid grid-cols-2 gap-2">
      <Button
        v-for="n in presets"
        :key="n"
        :data-preset-button="n"
        :data-preset-active="etat?.preset === n ? 'true' : undefined"
        :aria-current="etat?.preset === n ? 'true' : undefined"
        :variant="etat?.preset === n ? 'default' : 'outline'"
        class="h-14 justify-start gap-3 px-3 md:h-12"
        :disabled="grisees"
        @click="emit('choisir', n)"
      >
        <span class="w-6 text-left text-base font-bold" :class="etat?.preset === n ? '' : 'text-muted-foreground'">{{ n }}</span>
        <span v-if="nomDe(n)" class="truncate font-medium" data-preset-name>{{ nomDe(n) }}</span>
        <span v-if="etat?.preset === n" class="ml-auto size-2 shrink-0 rounded-full bg-current" aria-hidden="true" />
      </Button>
    </div>
  </div>
</template>
```

- [ ] **Step 4 : vérifier**

Run : `cd web/app && npx vitest run src/components/GrillePresets.test.ts` → PASS.

- [ ] **Step 5 : commit**

```bash
git add web/app/src/components/GrillePresets.vue web/app/src/components/GrillePresets.test.ts
git commit -m "feat(web): tuiles de preselections nommees, pagination extraite de HomeView"
```

---

### Task 9: `PlayerCard` — la pochette et le morceau au centre

**Files:**
- Modify: `web/app/src/components/PlayerCard.vue`
- Modify: `web/app/src/components/PlayerCard.test.ts`

**Interfaces:**
- Produit : mêmes props (`etat`, `pasDeplacement`) et émission `deplacer` ; deux slots : `actions` (rendu dans `CardAction` de l'en-tête — Source et Veille y vont depuis `HomeView`) et `commandes` (rendu sous la barre de progression — `Transport` et `Volume`). Les lignes « Source active : », « Présélection : », « Volume : » disparaissent ; `data-source` passe sur la pastille de l'en-tête, `data-volume` vit désormais dans `Volume.vue` (Tâche 6), `data-player-preset` reste sur le seul numéro, `data-player-preset-name` sur le nom. Les badges `data-origin` / `data-cover-origin` sont **conservés**, sous l'album.

- [ ] **Step 1 : adapter les tests**

Dans `PlayerCard.test.ts` :
- `'affiche source et volume des la premiere trame'` → renommer `'affiche la source dès la première trame'` et ne garder que `expect(w.get('[data-source]').text()).toBe('radio')` (le volume vit dans `Volume.vue`).
- Supprimer `'signale le muet et la veille'` → remplacer par `'signale la veille'` (garder l'assertion `data-standby`, retirer `data-muted`) ; supprimer `'n affiche ni muet ni veille quand ils sont inactifs'` sa moitié muet ; supprimer `'dit la sourdine sur la ligne du volume, et barre la valeur'` et `'suit les changements de volume sans rechargement'` (déplacés en `Volume.test.ts`).
- `'affiche la présélection en cours quand la Source en déclare une'` : l'assertion devient `expect(w.get('[data-player-preset]').text()).toBe('1')` (le préfixe « P » est hors du marqueur).
- Ajouter :

```ts
  it('la pochette et le morceau sont au centre, la source en pastille', () => {
    const w = monteAvec({ title: 'Blue in Green', artist: 'Miles Davis', album: 'Kind of Blue', preset: 1, preset_name: 'FIP' })
    expect(w.get('[data-source]').text()).toBe('radio')
    expect(w.get('[data-player-preset]').text()).toBe('1')
    expect(w.get('[data-player-preset-name]').text()).toBe('FIP')
    expect(w.get('[data-titre]').classes()).toContain('text-xl')
    expect(w.find('[data-pochette]').exists()).toBe(true)
  })

  it('le carre de pochette reste la meme sans morceau : c est lui qui tient la mise en page', () => {
    const w = monteAvec({ status: 'NO DISC', preset_count: 0 })
    expect(w.find('[data-pochette]').exists()).toBe(true)
    expect(w.get('[data-pochette-repli]').exists()).toBe(true)
    expect(w.get('[data-player-status]').text()).toBe('NO DISC')
  })

  it('en veille la pochette s eteint', () => {
    const w = monteAvec({ standby: true })
    expect(w.get('[data-pochette]').classes()).toContain('opacity-50')
    expect(w.get('[data-standby]').exists()).toBe(true)
  })

  it('rend les slots actions et commandes', () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 200 })))
    const w = mount(PlayerCard, {
      props: { etat: complet({}), pasDeplacement: 10 },
      slots: { actions: '<button data-test-action>a</button>', commandes: '<div data-test-commandes>c</div>' },
    })
    expect(w.find('[data-slot="card-action"] [data-test-action]').exists()).toBe(true)
    expect(w.find('[data-test-commandes]').exists()).toBe(true)
  })
```

Les tests existants sur `data-now-playing` (`'n affiche pas de bloc morceau tant que rien n est connu'`, `'retire le bloc morceau quand la lecture s arrete'`) : le marqueur `data-now-playing` reste posé sur le **bloc texte du morceau** (titre/artiste/album/badges), qui n'est rendu que si `!riendAfficher(etat)` — la pochette, elle, est toujours là. Vérifier que `'garde le carre en place quand il n y a pas de pochette'` reste vrai.

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/components/PlayerCard.test.ts` → FAIL sur les nouveaux tests.

- [ ] **Step 3 : réécrire le gabarit**

Garder le `<script setup>` existant (imports, `imageCassee`, `watch`, `emit`) et y ajouter `formateDuree` s'il n'y est pas déjà. Remplacer tout le `<template>` par :

```vue
<template>
  <!--
    La pochette et le morceau sont le sujet : c'est la seule chose qu'on
    regarde depuis le canape. L'etat (source, veille) tient dans l'en-tete ;
    le volume est le curseur du slot `commandes`. Sur telephone tout est
    centre en colonne ; a partir de `md` la pochette passe a gauche du texte.
  -->
  <Card data-player>
    <CardHeader class="pb-2">
      <CardTitle class="flex items-center gap-2 text-base">
        {{ t('player_title') }}
        <!-- La source en pastille : un badge du kit, `data-source` conserve
             pour les parcours. Le point vert dit « ca joue » (playback), la
             ou l'ancienne ligne de texte ne disait rien. -->
        <Badge variant="secondary" class="gap-1.5 font-normal">
          <span
            v-if="etat?.playback === 'playing'"
            class="size-1.5 rounded-full bg-primary"
            aria-hidden="true"
            data-lecture-en-cours
          />
          <span data-source>{{ etat ? etat.source || t('no_source') : '' }}</span>
        </Badge>
        <Badge v-if="etat?.standby" variant="secondary" data-standby>{{ t('standby') }}</Badge>
      </CardTitle>
      <CardAction v-if="$slots.actions">
        <slot name="actions" />
      </CardAction>
    </CardHeader>
    <CardContent class="flex flex-col items-center gap-4 md:flex-row md:items-start md:gap-5">
      <!-- Le carre est toujours la, image ou repli : c'est lui qui tient la
           mise en page, et une image qui arrive apres le texte ne doit rien
           decaler. 224 px sur telephone (le sujet), 176 px a cote du texte
           sur PC. -->
      <div
        class="size-56 shrink-0 overflow-hidden rounded-lg border border-border bg-muted shadow-md md:size-44"
        :class="{ 'opacity-50': etat?.standby }"
        data-pochette
      >
        <img
          v-if="etat?.cover_href && !imageCassee"
          :src="etat.cover_href"
          :alt="t('cover_alt')"
          class="size-full object-cover"
          @error="imageCassee = true"
        />
        <div
          v-else
          class="flex size-full items-center justify-center text-muted-foreground"
          data-pochette-repli
          aria-hidden="true"
        >
          <svg width="56" height="56" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M9 18V5l12-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="18" cy="16" r="3" />
          </svg>
        </div>
      </div>
      <div class="flex min-w-0 flex-1 flex-col items-center gap-1 text-center md:items-start md:text-left">
        <!-- La presélection en surligne : `P1 · FIP`. Absente quand la source
             n'en declare pas (cd sans disque, entree aux). -->
        <p v-if="etat?.preset != null" class="text-[11px] font-semibold uppercase tracking-wider text-primary">
          P<span data-player-preset>{{ etat.preset }}</span>
          <template v-if="etat.preset_name"> · <span data-player-preset-name>{{ etat.preset_name }}</span></template>
        </p>
        <!-- Le statut de la source (« PAS DE DISQUE »), masque en veille : le
             badge VEILLE porte deja le mot. -->
        <p v-if="etat?.status && !etat.standby" class="text-sm text-muted-foreground" data-player-status>
          {{ etat.status }}
        </p>
        <div v-if="!riendAfficher(etat)" class="flex min-w-0 flex-col items-center gap-0.5 md:items-start" data-now-playing>
          <p v-if="etat?.title" class="text-xl font-semibold leading-tight text-foreground" data-titre>{{ etat.title }}</p>
          <p v-if="etat?.artist" class="text-sm text-foreground" data-artiste>{{ etat.artist }}</p>
          <p v-if="etat?.album" class="text-sm text-muted-foreground" data-album>{{ etat.album }}</p>
          <!-- Qui a fourni le texte, et la pochette quand ce n'est pas le meme :
               la premiere question devant un titre faux. -->
          <div class="mt-1 flex items-center gap-1.5">
            <Badge v-if="etat?.origin" variant="secondary" class="text-[10px]" data-origin>{{ etat.origin }}</Badge>
            <Badge
              v-if="etat?.cover_origin && etat.cover_origin !== etat.origin"
              variant="secondary"
              class="text-[10px]"
              data-cover-origin
            >
              {{ etat.cover_origin }}
            </Badge>
            <span
              v-if="etat?.position_s == null && formateDuree(etat?.duration_s)"
              class="text-xs text-muted-foreground"
              :title="t('track_length')"
              data-duree
            >
              {{ formateDuree(etat?.duration_s) }}
            </span>
          </div>
        </div>
      </div>
    </CardContent>
    <CardContent class="space-y-3 pt-0">
      <BarreProgression
        :position="etat?.position_s ?? null"
        :duree="etat?.duration_s ?? null"
        :deplacable="etat?.seekable ?? false"
        :pas="pasDeplacement"
        @deplacer="(s) => emit('deplacer', s)"
      />
      <slot name="commandes" />
    </CardContent>
  </Card>
</template>
```

Retirer le `<script>` les imports devenus inutiles (`Badge` reste). La clé `now_playing` n'est plus employée dans ce composant : la laisser dans les catalogues (elle ne gêne pas ; le garde-fou ne teste que le sens « utilisée → présente »).

- [ ] **Step 4 : vérifier**

Run : `cd web/app && npx vitest run src/components/PlayerCard.test.ts` → PASS.

- [ ] **Step 5 : commit**

```bash
git add web/app/src/components/PlayerCard.vue web/app/src/components/PlayerCard.test.ts
git commit -m "feat(web): la pochette et le morceau au centre de la carte Lecteur"
```

---

### Task 10: `HomeView` — assemblage, deux colonnes à partir de `md`

**Files:**
- Modify: `web/app/src/views/HomeView.vue`
- Modify: `web/app/src/views/HomeView.test.ts`

**Interfaces:**
- Consomme : `PlayerCard` (slots `actions`, `commandes`), `Transport`, `Volume`, `GrillePresets`, `usePresets`, `REMOTE_POWER`, `REMOTE_SOURCE`, `REMOTE_MUTE`, `indisponible`.
- Produit : la page `/`. Commandes envoyées : `Select`, `SeekTo`, `SetVolume`, `Mute`, `Power`, `SourceCycle`, et celles du transport. Recharge `/api/presets` au montage et à chaque changement de `etat.source`.

- [ ] **Step 1 : adapter les tests**

Dans `HomeView.test.ts` :
- Supprimer le bloc `describe('HomeView — volume maintenu', …)` en entier.
- Dans `describe('HomeView', …)` : supprimer `'rend une rangée par groupe et la veille dans l’en-tête'` ; garder les autres (ils ciblent `data-preset-button`, `data-remote-power`, `data-player-preset`…). Le test `'la veille est dans le slot d’action de l’en-tête'` reste vrai (Source et Veille vont dans le slot `actions` de `PlayerCard`).
- Dans `describe('HomeView — boutons indisponibles', …)` : supprimer `'hors veille, seul le déplacement dépend du contenu'`, `'un contenu déplaçable rend les deux touches de déplacement'` ; remplacer `'Eject suit la source : grisé sur la radio, actif sur le lecteur de cd'` et `'un tiroir s’ouvre sans disque…'` par :

```ts
  it('Eject est masqué sur la radio et présent sur le lecteur de cd, disque ou pas', async () => {
    const { w, pousse } = await monter()
    pousse({ can_eject: false })
    await nextTick()
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(false)
    pousse({ can_eject: true, preset_count: 0, status: 'NO DISC' })
    await nextTick()
    expect(w.find('[data-remote-command="Eject"]').exists()).toBe(true)
  })
```

  (adapter `monter`/`pousse` au nom de l'aide déjà présente dans ce bloc — elle monte `HomeView` et pousse via `FauxEventSource.derniere.pousse`). Le test `'en veille, tout est grisé sauf la veille elle-même'` doit désormais vérifier `[data-remote-command]`, `[data-preset-button]`, `[data-remote-source]` grisés et `[data-remote-power]` actif.
- `'la touche Muet montre son état, là où on agit sur le volume'` : la cible est désormais `[data-remote-command="Mute"]` avec `aria-pressed="true"` et `data-actif="true"` — vérifier que les sélecteurs du test correspondent.
- Ajouter :

```ts
describe('HomeView — curseurs et noms', () => {
  it('le curseur de volume poste SetVolume, absolu', async () => {
    const posts: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') { posts.push(String(init.body)); return new Response(null, { status: 204 }) }
      if (url === '/api/presets') return new Response(JSON.stringify({ sources: [] }), { status: 200 })
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    FauxEventSource.derniere!.pousse({ volume: 60 })
    await nextTick()
    const poignee = w.get('[data-volume-curseur] [role="slider"]')
    ;(poignee.element as HTMLElement).focus()
    await poignee.trigger('keydown', { key: 'ArrowRight' })
    expect(posts).toContain(JSON.stringify({ cmd: 'SetVolume', arg: 61 }))
  })

  it('nomme les tuiles depuis /api/presets et recharge au changement de source', async () => {
    const gets: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') return new Response(null, { status: 204 })
      gets.push(url)
      if (url === '/api/presets') {
        return new Response(JSON.stringify({ sources: [
          { name: 'radio', presets: [{ index: 1, name: 'FIP' }] },
          { name: 'files', presets: [{ index: 1, name: 'tout.m3u' }] },
        ] }), { status: 200 })
      }
      return new Response(JSON.stringify({ seek_step_s: 10 }), { status: 200 })
    }))
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    await flushPromises()
    FauxEventSource.derniere!.pousse({ source: 'radio', preset_count: 3 })
    await nextTick()
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('FIP')
    const avant = gets.filter((u) => u === '/api/presets').length
    FauxEventSource.derniere!.pousse({ source: 'files', preset_count: 3 })
    await flushPromises()
    expect(gets.filter((u) => u === '/api/presets').length).toBe(avant + 1)
    expect(w.get('[data-preset-button="1"] [data-preset-name]').text()).toBe('tout.m3u')
  })
})
```

  Ajouter `flushPromises` à l'import de `@vue/test-utils`. Dans le stub global de `fetch` des tests existants qui montent `HomeView` (ils renvoient `{ seek_step_s: 10 }` à tout GET), la réponse sert aussi à `/api/presets` : `usePresets` lit `charge.sources.map` → ajouter au début de `recharger` (Tâche 4) une garde `if (!charge || !Array.isArray(charge.sources)) return`. La mettre **maintenant** dans `usePresets.ts` et compléter son test d'un cas « réponse sans `sources` : la liste précédente reste ».

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/views/HomeView.test.ts` → FAIL (`REMOTE_ROWS` n'existe plus, `data-volume-curseur` absent).

- [ ] **Step 3 : réécrire `HomeView.vue`**

```vue
<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, toast } from '@ritornello/ui'
import { LoopIcon } from '@radix-icons/vue'
import { onMounted, ref, watch } from 'vue'
import GrillePresets from '../components/GrillePresets.vue'
import IconeVeille from '../components/icones/IconeVeille.vue'
import PlayerCard from '../components/PlayerCard.vue'
import Transport from '../components/Transport.vue'
import Volume from '../components/Volume.vue'
import { useCatalog } from '../composables/useCatalog'
import { usePlayer } from '../composables/usePlayer'
import { usePresets } from '../composables/usePresets'
import type { Command, SettingsPayload } from '../types'
import { indisponible, REMOTE_MUTE, REMOTE_POWER, REMOTE_SOURCE } from './remoteCommands'

const { t } = useCatalog()

// L'unique connexion SSE de la page vit ici : la carte Lecteur, le transport,
// le volume et la grille consomment le meme etat, pousse par `/api/player`.
const { etat, ouvre } = usePlayer()
onMounted(ouvre)

async function send(cmd: Command) {
  const err = await api.post('/api/command', cmd)
  if (err) toast.error(err)
}

// Les noms des tuiles : charges au montage, recharges quand la source active
// change — c'est la trame qui le dit, rien n'est sonde.
const { recharger, nomDe } = usePresets()
onMounted(recharger)
watch(() => etat.value?.source, (apres, avant) => {
  if (apres !== undefined && apres !== avant) recharger()
})

// Pas de deplacement au clavier de la barre : celui des touches physiques,
// servi par /api/settings. Le defaut couvre le temps du GET et son echec.
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 800,
  volume_repeat_interval_ms: 200,
  startup_power: 'on',
  overlay_ms: 5000,
  tens_window_ms: 5000,
  cover_source_max_mio: 20,
  cover_rendition: true,
  cover_max_edge_px: 640,
  cover_jpeg_quality: 85,
  cover_max_bytes_ko: 512,
  cover_max_pixels_mpx: 16,
  seek_step_s: 10,
})
onMounted(async () => {
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
})
</script>

<template>
  <!-- Une colonne sur telephone ; deux cartes cote a cote a partir de `md`. -->
  <div class="grid gap-4 md:grid-cols-2 md:items-start">
    <PlayerCard
      :etat="etat"
      :pas-deplacement="reglages.seek_step_s"
      @deplacer="(s: number) => send({ cmd: 'SeekTo', arg: s })"
    >
      <!-- Les deux commandes qui portent sur l'appareil entier, au coin de la
           carte : la source, puis la veille au coin extreme. -->
      <template #actions>
        <div class="flex items-center gap-1">
          <Button
            variant="outline"
            size="sm"
            data-remote-source
            :disabled="indisponible(REMOTE_SOURCE.cmd.cmd, etat)"
            @click="send(REMOTE_SOURCE.cmd)"
          >
            <LoopIcon class="size-4" />
            {{ t(REMOTE_SOURCE.key) }}
          </Button>
          <Button variant="outline" size="icon-sm" data-remote-power :aria-label="t(REMOTE_POWER.key)" :title="t(REMOTE_POWER.key)" @click="send(REMOTE_POWER.cmd)">
            <IconeVeille class="size-4" />
          </Button>
        </div>
      </template>
      <template #commandes>
        <Transport :etat="etat" @commande="send" />
        <Volume
          :volume="etat?.volume ?? null"
          :muted="etat?.muted ?? false"
          :desactive="indisponible(REMOTE_MUTE.cmd.cmd, etat)"
          @regler="(v: number) => send({ cmd: 'SetVolume', arg: v })"
          @muet="send(REMOTE_MUTE.cmd)"
        />
      </template>
    </PlayerCard>
    <Card>
      <CardHeader><CardTitle>{{ t('presets_label') }}</CardTitle></CardHeader>
      <CardContent>
        <GrillePresets :etat="etat" :nom-de="(n: number) => (etat ? nomDe(etat.source, n) : null)" @choisir="(n: number) => send({ cmd: 'Select', arg: n })" />
      </CardContent>
    </Card>
  </div>
</template>
```

La clé `remote_title` n'est plus employée : la laisser dans les catalogues.

- [ ] **Step 4 : vérifier toute la suite web**

Run : `cd web/app && npx vitest run && npx vue-tsc --noEmit && npx vitest run src/i18nKeysUsed.test.ts`
Expected : PASS. Les blocs `'pagination des présélections'` et `'la page suit ce qui joue'` passent inchangés sur la nouvelle page.

- [ ] **Step 5 : commit**

```bash
git add web/app/src/views/HomeView.vue web/app/src/views/HomeView.test.ts web/app/src/composables/usePresets.ts web/app/src/composables/usePresets.test.ts
git commit -m "feat(web): telecommande recomposee, deux colonnes a partir de md"
```

---

### Task 11: Navigation — barre basse fixe et liste `/plugins/`

**Files:**
- Create: `web/app/src/components/NavBasse.vue`, `web/app/src/views/PluginsView.vue`
- Modify: `web/app/src/App.vue`, `web/app/src/router.ts`
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`
- Test: `web/app/src/components/NavBasse.test.ts`, `web/app/src/views/PluginsView.test.ts`, `web/app/src/router.test.ts`, `web/app/src/App.test.ts`

**Interfaces:**
- Consomme : `useGreffons().admins` (ordre de `/api/status`), `useGreffons().etat.plugins` (badges).
- Produit : `<NavBasse />` — 4 onglets `Écoute · Greffons · Système · Réglages`, `md:hidden`, `data-nav-basse` ; l'onglet Greffons pointe sur `/plugins/` (ou directement `/plugins/<nom>/` quand il n'y a qu'un greffon), `data-nav-greffons`. Route `/plugins/` (`name: 'plugins'`, `strict: true`) → `PluginsView` (`data-plugins-liste`, un `RouterLink` par greffon). Clés i18n : `nav_listen`, `nav_plugins`, `nav_settings`, `plugins_list_title`, `plugins_list_empty`.

- [ ] **Step 1 : les tests qui échouent**

`NavBasse.test.ts` :

```ts
import { mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

async function monter(status: { plugins: { name: string; kind: string; connected: boolean; admin: boolean }[]; active_source: string }) {
  vi.resetModules()
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify(status), { status: 200 })))
  const { useGreffons } = await import('../composables/useGreffons')
  await useGreffons().rafraichir()
  const NavBasse = (await import('./NavBasse.vue')).default
  const router = createRouter({ history: createMemoryHistory(), routes: [
    { path: '/', component: { template: '<div />' } },
    { path: '/plugins/', component: { template: '<div />' } },
    { path: '/plugins/:name/', component: { template: '<div />' } },
    { path: '/system', component: { template: '<div />' } },
    { path: '/config', component: { template: '<div />' } },
  ] })
  await router.push('/')
  await router.isReady()
  return mount(NavBasse, { global: { plugins: [router] } })
}

describe('NavBasse', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('quatre onglets, toujours, quel que soit le nombre de greffons', async () => {
    const w = await monter({ plugins: [
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'generic-input', kind: 'input', connected: true, admin: true },
    ], active_source: 'radio' })
    expect(w.findAll('[data-nav-basse] a')).toHaveLength(4)
    expect(w.get('[data-nav-greffons]').attributes('href')).toBe('/plugins/')
  })

  it('un seul greffon : l’onglet mène directement à sa page', async () => {
    const w = await monter({ plugins: [{ name: 'radio', kind: 'source', connected: true, admin: true }], active_source: 'radio' })
    expect(w.findAll('[data-nav-basse] a')).toHaveLength(4)
    expect(w.get('[data-nav-greffons]').attributes('href')).toBe('/plugins/radio/')
  })

  it('aucun greffon à page : l’onglet mène à la liste, qui dira qu’elle est vide', async () => {
    const w = await monter({ plugins: [], active_source: '' })
    expect(w.get('[data-nav-greffons]').attributes('href')).toBe('/plugins/')
  })
})
```

`PluginsView.test.ts` :

```ts
import { mount } from '@vue/test-utils'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

async function monter(plugins: object[]) {
  vi.resetModules()
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ plugins, active_source: 'radio' }), { status: 200 })))
  const { useGreffons } = await import('../composables/useGreffons')
  await useGreffons().rafraichir()
  const PluginsView = (await import('./PluginsView.vue')).default
  const router = createRouter({ history: createMemoryHistory(), routes: [{ path: '/plugins/:name/', component: { template: '<div />' } }, { path: '/', component: { template: '<div />' } }] })
  return mount(PluginsView, { global: { plugins: [router] } })
}

describe('PluginsView', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('liste les greffons à page d’admin, dans l’ordre de /api/status, sans doublon', async () => {
    const w = await monter([
      { name: 'files', kind: 'source', connected: true, admin: true },
      { name: 'radio', kind: 'source', connected: true, admin: true },
      { name: 'mpd', kind: 'input', connected: true, admin: true },
      { name: 'mpd', kind: 'display', connected: true, admin: true },
      { name: 'console', kind: 'display', connected: true, admin: false },
    ])
    const liens = w.findAll('[data-plugins-liste] a')
    expect(liens.map((a) => a.attributes('href'))).toEqual(['/plugins/files/', '/plugins/radio/', '/plugins/mpd/'])
  })

  it('dit quand aucun greffon n’a de page', async () => {
    const w = await monter([])
    expect(w.find('[data-plugins-vide]').exists()).toBe(true)
  })
})
```

`router.test.ts` — ajouter :

```ts
  it('/plugins/ est la liste des greffons, distincte de /plugins/<nom>/', async () => {
    await router.push('/plugins/')
    expect(router.currentRoute.value.name).toBe('plugins')
    await router.push('/plugins/radio/')
    expect(router.currentRoute.value.name).toBe('plugin')
  })
```

(adapter au `router` importé dans ce fichier).

`App.test.ts` — ajouter :

```ts
  it('la nav du haut est masquée sous md, la barre basse rendue', async () => {
    const w = await monterApp() // l'aide de montage existante du fichier
    expect(w.get('[data-nav-haut]').classes()).toContain('hidden')
    expect(w.get('[data-nav-haut]').classes()).toContain('md:flex')
    expect(w.find('[data-nav-basse]').exists()).toBe(true)
  })
```

- [ ] **Step 2 : vérifier l'échec**

Run : `cd web/app && npx vitest run src/components/NavBasse.test.ts src/views/PluginsView.test.ts src/router.test.ts src/App.test.ts` → FAIL.

- [ ] **Step 3 : i18n**

`en.toml`, à la suite de `system_title` :

```toml
nav_listen = "Listen"
nav_plugins = "Plugins"
nav_settings = "Settings"
plugins_list_title = "Plugin pages"
plugins_list_empty = "No plugin offers a page."
```

`deploy/locales/core/fr.toml` :

```toml
nav_listen = "Écoute"
nav_plugins = "Greffons"
nav_settings = "Réglages"
plugins_list_title = "Pages des greffons"
plugins_list_empty = "Aucun greffon ne propose de page."
```

- [ ] **Step 4 : la route et la liste**

`router.ts`, **avant** la route `/plugins/:name/` :

```ts
    // La liste des pages de greffons : la cible de l'onglet « Greffons » de la
    // barre basse, qui a besoin d'une destination fixe quel que soit le nombre
    // de greffons. `strict` pour ne pas matcher `/plugins` nu.
    { path: '/plugins/', name: 'plugins', strict: true, component: () => import('./views/PluginsView.vue') },
```

`PluginsView.vue` :

```vue
<script setup lang="ts">
import { Badge, Card, CardContent, CardHeader, CardTitle } from '@ritornello/ui'
import { ChevronRightIcon } from '@radix-icons/vue'
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import { useGreffons } from '../composables/useGreffons'

// La partie variable de la navigation, rangee dans une liste : sur telephone
// la barre basse a quatre onglets fixes, et c'est ici qu'atterrit « Greffons ».
// Meme source et meme ordre que les liens du haut (`useGreffons().admins`, donc
// `/api/status`, donc plugins.toml) — aucune priorite deduite ailleurs.
const { t } = useCatalog()
const { admins, etat } = useGreffons()
const connecte = (nom: string) => etat.value.plugins.some((p) => p.name === nom && p.connected)
const liste = computed(() => admins.value)
</script>

<template>
  <Card>
    <CardHeader><CardTitle>{{ t('plugins_list_title') }}</CardTitle></CardHeader>
    <CardContent>
      <ul v-if="liste.length" class="divide-y divide-border" data-plugins-liste>
        <li v-for="name in liste" :key="name">
          <RouterLink :to="`/plugins/${name}/`" class="flex min-h-14 items-center gap-3 py-2 hover:text-foreground">
            <span class="flex-1 font-medium first-letter:uppercase">{{ name }}</span>
            <Badge v-if="!connecte(name)" variant="secondary">{{ t('plugin_disconnected') }}</Badge>
            <ChevronRightIcon class="size-4 text-muted-foreground" />
          </RouterLink>
        </li>
      </ul>
      <p v-else class="text-sm text-muted-foreground" data-plugins-vide>{{ t('plugins_list_empty') }}</p>
    </CardContent>
  </Card>
</template>
```

Vérifier que `plugin_disconnected` existe dans `en.toml` (`grep -n plugin_disconnected crates/ritornello-core/src/locales/en.toml`) ; sinon employer la clé que `ConfigView.vue` utilise pour son badge d'état (la lire dans `ConfigView.vue`, ligne ~270-300) — même mot aux deux endroits.

- [ ] **Step 5 : la barre basse**

`NavBasse.vue` :

```vue
<script setup lang="ts">
import { ActivityLogIcon, CubeIcon, MixerHorizontalIcon, PlayIcon } from '@radix-icons/vue'
import { computed } from 'vue'
import { RouterLink } from 'vue-router'
import { useCatalog } from '../composables/useCatalog'
import { useGreffons } from '../composables/useGreffons'

// Quatre onglets fixes : la partie variable (une page par greffon) est derriere
// « Greffons ». Un seul greffon a page : l'onglet y mene tout droit, la liste
// n'apporterait rien.
const { t } = useCatalog()
const { admins } = useGreffons()
const cibleGreffons = computed(() => (admins.value.length === 1 ? `/plugins/${admins.value[0]}/` : '/plugins/'))

const ONGLET = 'flex h-14 flex-col items-center justify-center gap-1 text-[11px] font-medium text-muted-foreground'
const ACTIF = 'text-primary'
</script>

<template>
  <!-- `fixed` et non `sticky` : le `main` defile sous elle, et le
       `safe-area-inset-bottom` la degage de la barre gestuelle du telephone.
       Masquee a partir de `md`, ou la nav du haut reprend. -->
  <nav
    class="fixed inset-x-0 bottom-0 z-10 grid grid-cols-4 border-t border-border bg-card pb-[env(safe-area-inset-bottom)] md:hidden"
    data-nav-basse
    :aria-label="t('nav_listen')"
  >
    <RouterLink to="/" :class="ONGLET" :exact-active-class="ACTIF">
      <PlayIcon class="size-5" />{{ t('nav_listen') }}
    </RouterLink>
    <!-- `active-class` inclusif ici : la page d'un greffon garde l'onglet allume. -->
    <RouterLink :to="cibleGreffons" :class="ONGLET" :active-class="ACTIF" data-nav-greffons>
      <CubeIcon class="size-5" />{{ t('nav_plugins') }}
    </RouterLink>
    <RouterLink to="/system" :class="ONGLET" :exact-active-class="ACTIF">
      <ActivityLogIcon class="size-5" />{{ t('system_title') }}
    </RouterLink>
    <RouterLink to="/config" :class="ONGLET" :exact-active-class="ACTIF">
      <MixerHorizontalIcon class="size-5" />{{ t('nav_settings') }}
    </RouterLink>
  </nav>
</template>
```

`App.vue` : envelopper les trois `RouterLink` `/config`, `/system` et `v-for admins` dans `<div class="hidden items-center gap-4 md:flex" data-nav-haut>…</div>` (la marque et `ThemeToggle` restent hors de l'enveloppe) ; `main` devient `class="mx-auto max-w-5xl px-4 py-6 pb-24 md:pb-6"` ; ajouter `<NavBasse />` après `</main>` et son import.

- [ ] **Step 6 : vérifier**

Run : `cd web/app && npx vitest run && npx vue-tsc --noEmit` ; (WSL) `cargo test -p ritornello-core parite`
Expected : PASS partout.

- [ ] **Step 7 : commit**

```bash
git add web/app/src/components/NavBasse.vue web/app/src/components/NavBasse.test.ts web/app/src/views/PluginsView.vue web/app/src/views/PluginsView.test.ts web/app/src/router.ts web/app/src/router.test.ts web/app/src/App.vue web/app/src/App.test.ts crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): barre d'onglets basse a quatre entrees, liste des greffons"
```

---

### Task 12: e2e — le parcours téléphone

**Files:**
- Modify: `web/app/playwright.config.ts`
- Create: `web/app/e2e/telephone.spec.ts`
- Vérifier: `web/app/e2e/parcours.spec.ts` (aucun sélecteur retiré n'y est employé — vérifié le 2026-08-26 : il n'utilise que `data-preset-*`, `data-source`, `data-volume`, `data-now-playing`, qui sont conservés)

**Interfaces:**
- Consomme : le harnais `e2e/serve.mjs` (cœur jetable, station FIP en présélection 1, volume > 0).
- Produit : un second projet Playwright `telephone` (`devices['Pixel 7']`) qui n'exécute que `telephone.spec.ts`.

- [ ] **Step 1 : configurer deux projets**

Dans `playwright.config.ts`, remplacer `use: { baseURL: 'http://127.0.0.1:8099', ...devices['Desktop Chrome'] },` par :

```ts
  use: { baseURL: 'http://127.0.0.1:8099' },
  // Deux viewports, un seul cœur : les parcours historiques sur bureau, et le
  // parcours téléphone qui vérifie la barre basse et les curseurs au doigt.
  // `workers: 1` ci-dessus vaut pour les deux projets, pour la même raison.
  projects: [
    { name: 'bureau', use: { ...devices['Desktop Chrome'] }, testIgnore: '**/telephone.spec.ts' },
    { name: 'telephone', use: { ...devices['Pixel 7'] }, testMatch: '**/telephone.spec.ts' },
  ],
```

- [ ] **Step 2 : le parcours**

`web/app/e2e/telephone.spec.ts` :

```ts
import { expect, test } from '@playwright/test'

test('sur téléphone : barre basse, nav du haut absente, tuile nommée', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-nav-basse]')).toBeVisible()
  await expect(page.locator('[data-nav-haut]')).toBeHidden()
  await expect(page.locator('[data-nav-basse] a')).toHaveCount(4)
  // La présélection 1 (FIP, stations.toml du harnais) joue et porte son nom.
  const tuile = page.locator('[data-preset-button="1"]')
  await expect(tuile).toHaveAttribute('aria-current', 'true')
  await expect(tuile.locator('[data-preset-name]')).toHaveText('FIP')
  // Le transport n'a ni ±10 s ni volume pas à pas, et pas d'Éjecter sur la radio.
  await expect(page.locator('[data-remote-command="Eject"]')).toHaveCount(0)
  await expect(page.locator('[data-remote-command]')).toHaveCount(5) // Prev PlayPause Next Stop Mute
})

test('sur téléphone : glisser le curseur de volume envoie un SetVolume que le cœur renvoie', async ({ page }) => {
  // La preuve de bout en bout du Slider : un vrai geste tactile, une seule
  // commande au relâchement, et la trame SSE qui revient avec la valeur.
  // Le volume plutôt que la progression parce que la radio n'est pas
  // `seekable` : la barre y est informative, sans poignée — ce qui se vérifie
  // aussi.
  await page.goto('/')
  await expect(page.locator('[data-volume]')).not.toHaveText('')
  await expect(page.locator('[data-barre] [role="slider"]')).toHaveCount(0)
  const curseur = page.locator('[data-volume-curseur]')
  const boite = await curseur.boundingBox()
  if (!boite) throw new Error('curseur de volume invisible')
  const y = boite.y + boite.height / 2
  await page.mouse.move(boite.x + boite.width * 0.5, y)
  await page.mouse.down()
  await page.mouse.move(boite.x + boite.width * 0.25, y, { steps: 5 })
  await page.mouse.up()
  // Entre 20 et 30 % : la position exacte dépend du padding de la poignée.
  await expect(page.locator('[data-volume]')).toHaveText(/^(2\d|30) %$/)
  const trame = await page.evaluate(
    () =>
      new Promise<{ volume: number }>((resolve) => {
        const flux = new EventSource('/api/player')
        flux.onmessage = (e) => { flux.close(); resolve(JSON.parse(e.data as string)) }
      }),
  )
  expect(trame.volume).toBeGreaterThanOrEqual(20)
  expect(trame.volume).toBeLessThanOrEqual(30)
})

test('sur téléphone : l’onglet Greffons mène à la liste, qui mène à la page du greffon', async ({ page }) => {
  await page.goto('/')
  await page.locator('[data-nav-greffons]').click()
  // Trois greffons à page dans le harnais (radio, files, generic-input) : la liste.
  await expect(page).toHaveURL(/\/plugins\/$/)
  await expect(page.locator('[data-plugins-liste] a')).toHaveCount(3)
  await page.locator('[data-plugins-liste] a').first().click()
  await expect(page).toHaveURL(/\/plugins\/radio\/$/)
})
```

- [ ] **Step 3 : lancer**

Run (WSL, une fois) : `cargo build --workspace`. Puis : `cd web/app && npx playwright test`
Expected : les projets `bureau` et `telephone` passent. Si le geste à la souris ne bouge pas la poignée (Playwright émule le tactile sur `Pixel 7` : `hasTouch: true`), remplacer les quatre `page.mouse.*` par un `await page.touchscreen.tap(boite.x + boite.width * 0.25, y)` — un tap est un pointerdown+up au même point, donc une validation à 25 % — et garder l'attente `/^(2\d|30) %$/`.

- [ ] **Step 4 : commit**

```bash
git add web/app/playwright.config.ts web/app/e2e/telephone.spec.ts
git commit -m "test(e2e): parcours telephone, barre basse et curseur de volume"
```

---

### Task 13: Documentation — `interface.md` et captures regénérées

**Files:**
- Modify: `docs/interface.md` (section « Web remote and command API », lignes 3-268)
- Create: `web/app/scripts/captures.mjs`
- Replace: `docs/captures/accueil-clair.png`, `docs/captures/accueil-sombre.png`, `docs/captures/admin-radio.png` ; Create: `docs/captures/accueil-telephone.png`
- Modify: `docs/development.md` (§ « Embedded data to regenerate » : une puce pour les captures)

- [ ] **Step 1 : le script de captures**

`web/app/scripts/captures.mjs` (suppose un cœur qui répond sur `127.0.0.1:8099` — `node e2e/serve.mjs` dans un autre terminal, arrêté ensuite par `node -e "import('./e2e/teardown.mjs').then(m=>m.default())"`) :

```js
// Regenere docs/captures/*.png depuis un coeur en marche (voir docs/development.md).
// Les captures a la main vieillissaient a chaque chantier ; celles-ci se
// refont en une commande, aux deux modes et aux deux largeurs.
import { chromium } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { resolve } from 'node:path'

const BASE = process.env.RITORNELLO_URL ?? 'http://127.0.0.1:8099'
const OUT = resolve(process.cwd(), '../../docs/captures')
mkdirSync(OUT, { recursive: true })

const navigateur = await chromium.launch()
async function capture(nom, { largeur, hauteur, mode, chemin = '/' }) {
  const page = await navigateur.newPage({ viewport: { width: largeur, height: hauteur }, deviceScaleFactor: 2 })
  await page.goto(`${BASE}/`)
  await page.waitForSelector('[data-preset-button]')
  // Le mode est un reglage de l'appareil (PUT /api/theme), pas du navigateur.
  const theme = await page.evaluate(() => fetch('/api/theme').then((r) => r.json()))
  await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), { ...theme, mode })
  await page.goto(`${BASE}${chemin}`)
  await page.waitForTimeout(800)
  await page.screenshot({ path: resolve(OUT, `${nom}.png`), fullPage: false })
  await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), theme)
  await page.close()
}

await capture('accueil-clair', { largeur: 1280, hauteur: 800, mode: 'light' })
await capture('accueil-sombre', { largeur: 1280, hauteur: 800, mode: 'dark' })
await capture('accueil-telephone', { largeur: 390, hauteur: 844, mode: 'light' })
await capture('admin-radio', { largeur: 1280, hauteur: 800, mode: 'light', chemin: '/plugins/radio/' })
await navigateur.close()
console.log(`captures ecrites dans ${OUT}`)
```

Run : `cd web/app && node e2e/serve.mjs` (terminal 1, attendre `web interface on`) ; `node scripts/captures.mjs` (terminal 2) ; puis le teardown. Regarder les quatre PNG (outil Read) : barre basse visible sur `accueil-telephone`, deux colonnes sur `accueil-clair`.

- [ ] **Step 2 : `docs/interface.md`**

Réécrire la section « Web remote and command API » pour décrire : la carte Lecteur (pastille source, surligne `P1 · FIP`, pochette 224/176 px, badges d'origine, barre de progression à trois états — inconnue / informative / curseur — et sa règle « un seul `SeekTo` au relâchement, valeur tenue jusqu'à la trame qui la rejoint ») ; le transport (`|◀ ▶/❚❚ ▶|`, `■`, `⏏` masqué sans `can_eject`, icône de lecture sur `playback`) ; le volume (curseur → `SetVolume`, haut-parleur = Muet, clavier flèches/Page) ; les tuiles nommées (`GET /api/presets`, rechargé au changement de source, fenêtres 1-9 / 10k-10k+9 inchangées) ; la navigation (barre basse à quatre onglets sous `md`, `Greffons` → `/plugins/` ou lien direct ; liens du haut à partir de `md`). Retirer les paragraphes sur `Rewind`/`Fast forward` et `Volume +/-` **côté web** (les garder dans « Physical remote »). Ajouter `GET /api/presets` à la liste des routes avec un exemple de réponse. Référencer les quatre captures.

- [ ] **Step 3 : `docs/development.md`**

Dans « Embedded data to regenerate », ajouter :

```markdown
- **Screenshots** (`docs/captures/*.png`): with a core running (`node
  e2e/serve.mjs` from `web/app`), `node scripts/captures.mjs` from `web/app`;
  then stop the core with the e2e teardown.
```

- [ ] **Step 4 : commit**

```bash
git add docs/interface.md docs/development.md docs/captures web/app/scripts/captures.mjs
git commit -m "docs(interface): la telecommande refondue, captures regenerees par script"
```

---

### Task 14: Vérification finale et bilan

- [ ] **Step 1 : tout au vert**

Run : `cd web/kit && npx vitest run` ; `cd web/app && npx vitest run && npx vue-tsc --noEmit` ; `npx playwright test` ; (WSL) `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`.
Expected : tout passe. Noter toute déviation dans le commit final ou dans un message au propriétaire.

- [ ] **Step 2 : relire la spec contre le résultat**

Points à cocher en regardant l'écran (cœur e2e, deux largeurs, mode sombre aussi) :
- aucune couleur en dur (basculer sur un preset très différent de `northern-lights`, ex. `vercel`, et vérifier que tout suit) ;
- cibles ≥ 44 px sur téléphone (boutons du transport 48-64 px, tuiles 56 px, curseurs 44 px, onglets 56 px) ;
- badges d'origine visibles quand un greffon de métadonnées parle (à défaut de greffon dans le harnais, vérifier par le test unitaire `PlayerCard`) ;
- barre de progression informative sur la radio (pas de poignée), curseur sur `files` (glisser puis lire la position).

- [ ] **Step 3 : préparer la fusion**

Ne pas fusionner depuis le worktree (mémoire : `ExitWorktree(keep)` puis `git merge --ff-only` depuis le dépôt principal). Proposer au propriétaire un squash par grande fonction : `core` (Tâche 1), `kit` (2), `web` (3-11), `e2e` (12), `docs` (13).

---

## Écarts assumés par rapport à la spec (à valider par le propriétaire)

1. **Fenêtres de présélections** : la spec disait « 6 sur téléphone, 10 sur PC ». Le plan garde les fenêtres **1-9 puis 10k…10k+9** existantes : ce sont celles du `+10` de la télécommande physique et du cœur, et les tests « la page suit ce qui joue » en dépendent. Sur téléphone, 9 tuiles en 2 colonnes font 5 rangées de 56 px — ça tient sous la pochette sans défiler sur un écran de 844 px.
2. **Mot derrière le compte** (« 12 stations » / « 12 pistes ») : le `Catalogue` ne porte pas le genre des présélections ; la carte dit « Présélections : 12 » avec la clé existante `presets_label`. Pas de nouvelles clés `presets_count_*`.
3. **Icône de la pastille source** : pas d'icône par source (aucun moyen de la choisir pour un greffon tiers) ; un point `bg-primary` quand `playback === 'playing'` en tient lieu.
4. **e2e du glisser** : sur le curseur de **volume** (toujours présent) plutôt que sur la progression (la radio du harnais n'est pas `seekable`) ; la progression déplaçable est couverte par les tests unitaires et par la vérification à l'écran de la Tâche 14.
5. **Compte de présélections à zéro** : `preset_count === 0` affiche le compte « 0 » dans la carte Présélections plutôt que le statut de la source (« NO DISC », etc.) — ce statut a sa propre ligne, dans la carte Lecteur.
6. **Mise en forme du muet** : seule la valeur numérique est barrée et assombrie (`line-through opacity-60`) quand `muted` ; la piste du curseur de volume elle-même n'est pas touchée.
7. **Tailles retenues** : bouton Play/Pause 64 px (48 px à partir de `md`) ; pochette 224 px sur téléphone, 176 px à côté du texte à partir de `md`.
8. **Clés i18n des curseurs** : pas de nouvelles clés `*_slider_label` — l'`aria-label` de la barre réutilise `position_label`, celui du volume réutilise `volume`.
9. **Couleur des icônes de plateformes** : décision du propriétaire, chaque icône (YouTube, Deezer, Apple Music) garde en dur la couleur officielle de sa marque, dans les deux palettes — la seule exception assumée à « aucune couleur en dur » dans cette IHM. Voir docs/interface.md, § Player card.
