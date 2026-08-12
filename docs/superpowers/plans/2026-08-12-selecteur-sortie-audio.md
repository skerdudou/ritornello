# Audio Output Selector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the audio output selector honest and safe: human-readable descriptions, the `null` ALSA device filtered out, and a "System default" entry that resets the choice (`audio_device: None`, `audio-device=auto` to mpv) instead of preselecting an arbitrary first device.

**Architecture:** `parse_device_list` keeps the indented `aplay -L` lines as descriptions and drops `null`; `GET /api/audio-output` serves `{name, description}` pairs; `PUT` accepts `{"device": null}`, carried as `Option<String>` through the audio channel to `Core::set_audio_device`, which sends `auto` to mpv and persists `None`. The SPA renders description-first entries plus a first, synthetic "System default" item mapped to `null` on save. Spec: `docs/superpowers/specs/2026-08-12-selecteur-sortie-audio-design.md`.

**Tech Stack:** Rust (axum, tokio, serde), Vue 3 + Vitest, Playwright (unchanged).

## Global Constraints

- Build order is always **npm then cargo** (SPA embedded at compile time by `crates/ritornello-core/build.rs`); when cargo runs through WSL on this Windows checkout, `touch crates/ritornello-core/build.rs` after an npm rebuild to force re-embedding.
- `cargo`/`clippy` run ONLY through WSL from PowerShell: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && cargo test ..."`. `git`, `npm`, `node`, Playwright run on Windows via PowerShell. The Bash tool is broken in this session — PowerShell only.
- Description separator: indented `aplay -L` lines trimmed and joined with `" — "` (em dash, spaces); empty string when a device has no indented lines.
- The filtered device is exactly the PCM named `null` (exact match); nothing else is filtered.
- Wire shapes: GET `{"devices": [{"name": "...", "description": "..."}], "current": "name"|null}`; PUT accepts `{"device": "name"}` (non-empty, else 422 `{"error": ...}` as today) or `{"device": null}` (= system default). The empty string stays refused.
- "System default": UI sentinel is the literal string `__system_default__` (view-level only, never sent); i18n key `audio_default_device` = `"System default"` (en) / `"Par défaut (système)"` (fr) in BOTH `crates/ritornello-core/src/locales/en.toml` and `deploy/locales/core/fr.toml`.
- `state.json`: choosing the default writes `"audio_device": null` (like the other optionals — no `skip_serializing_if`).
- Comments in English in Rust, French in the SPA (each file's existing idiom); French test names; French user-facing diagnostic strings.
- No git push (no remote). Commit after every task. Integration to `main` at the end is `git merge --ff-only` (controller does it after the final whole-branch review).

---

### Task 0: Worktree build environment

The worktree `C:\projets\perso\ritornello\.claude\worktrees\selecteur-sortie-audio` is fresh: no `node_modules`, no `dist`, no `target`.

**Files:** none (environment only).

- [ ] **Step 1: Install and build the web workspaces (Windows)**

```powershell
npm ci
npm run build --workspaces
```
Expected: all workspaces build; the dist verifiers pass.

- [ ] **Step 2: Prime the Rust build (WSL)**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && cargo test --workspace 2>&1 | tail -5"
```
Expected: full suite passes (baseline green before any change).

---

### Task 1: Descriptions kept, `null` filtered, GET serves pairs

**Files:**
- Modify: `crates/ritornello-core/src/audio_output.rs`
- Modify: `crates/ritornello-core/src/status.rs` (GET response type only)

**Interfaces:**
- Produces: `pub struct AudioDevice { pub name: String, pub description: String }` (`Debug, Clone, PartialEq, Eq, serde::Serialize`); `parse_device_list(&str) -> Vec<AudioDevice>`; `list_devices() -> Result<Vec<AudioDevice>>`; `GET /api/audio-output` → `{"devices": [{"name", "description"}], "current": ...}`.

- [ ] **Step 1: Write the failing tests**

Replace the two existing tests in `audio_output.rs` with:

```rust
    #[test]
    fn garde_les_descriptions_et_filtre_null() {
        let raw = "null\n    Discard all samples (playback) or generate zero samples (capture)\n\
default\n    Playback/recording through the PulseAudio sound server\n\
sysdefault:CARD=Headphones\n    bcm2835 Headphones, bcm2835 Headphones\n    Default Audio Device\n";
        let devices = parse_device_list(raw);
        assert_eq!(
            devices,
            vec![
                AudioDevice {
                    name: "default".into(),
                    description: "Playback/recording through the PulseAudio sound server".into(),
                },
                AudioDevice {
                    name: "sysdefault:CARD=Headphones".into(),
                    description: "bcm2835 Headphones, bcm2835 Headphones — Default Audio Device".into(),
                },
            ]
        );
    }

    #[test]
    fn peripherique_sans_description_et_entree_vide() {
        assert_eq!(
            parse_device_list("hw:CARD=Loopback\n"),
            vec![AudioDevice { name: "hw:CARD=Loopback".into(), description: String::new() }]
        );
        assert_eq!(parse_device_list(""), Vec::<AudioDevice>::new());
    }
```

In `status.rs` tests, update `get_audio_output_liste_les_peripheriques_et_la_selection` to assert the new shape:

```rust
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["current"], "default");
        assert!(v["devices"].is_array());
        // Chaque périphérique est une paire nom/description, plus une chaîne nue.
        if let Some(premier) = v["devices"].get(0) {
            assert!(premier["name"].is_string());
            assert!(premier["description"].is_string());
        }
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && cargo test -p ritornello-core audio_output 2>&1 | tail -5"
```
Expected: FAIL to compile (`AudioDevice` not found).

- [ ] **Step 3: Implement**

`audio_output.rs` becomes:

```rust
use anyhow::{bail, Result};

/// One selectable ALSA PCM, as listed by `aplay -L`: the technical name and
/// the human-readable description the SPA shows first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub description: String,
}

/// Parses `aplay -L`: each non-indented line names a selectable PCM; the
/// indented lines under it are its description (kept, trimmed, joined with
/// " — "). The `null` PCM is filtered out: it discards audio — useless in an
/// audio chain, and it used to sit first in the list where the SPA's old
/// preselection fallback could send it on a distracted "Change" click.
pub fn parse_device_list(raw: &str) -> Vec<AudioDevice> {
    let mut devices: Vec<AudioDevice> = Vec::new();
    // While skipping `null`, its own indented lines must not leak into the
    // previous device's description.
    let mut skipping = false;
    for line in raw.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            if line.trim().is_empty() {
                continue;
            }
            let name = line.trim().to_string();
            skipping = name == "null";
            if !skipping {
                devices.push(AudioDevice { name, description: String::new() });
            }
        } else if !skipping {
            if let Some(d) = devices.last_mut() {
                let part = line.trim();
                if !part.is_empty() {
                    if !d.description.is_empty() {
                        d.description.push_str(" — ");
                    }
                    d.description.push_str(part);
                }
            }
        }
    }
    devices
}

pub fn list_devices() -> Result<Vec<AudioDevice>> {
    let out = std::process::Command::new("aplay").arg("-L").output()?;
    if !out.status.success() {
        bail!("aplay -L a echoue: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(parse_device_list(&String::from_utf8_lossy(&out.stdout)))
}
```

In `status.rs`, the GET response struct becomes:

```rust
#[derive(Serialize)]
struct AudioOutputResponse {
    devices: Vec<crate::audio_output::AudioDevice>,
    current: Option<String>,
}
```
(`audio_output_json` itself is unchanged — `list_devices()` now returns the pairs.)

- [ ] **Step 4: Run the crate suite**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && cargo test -p ritornello-core 2>&1 | tail -5"
```
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add crates/ritornello-core/src/audio_output.rs crates/ritornello-core/src/status.rs
git commit -m "feat(core): sorties audio avec description, peripherique null filtre"
```

---

### Task 2: Nullable device end to end (PUT null → auto to mpv → None persisted)

**Files:**
- Modify: `crates/ritornello-core/src/core.rs` (`set_audio_device`)
- Modify: `crates/ritornello-core/src/main.rs` (audio channel type, select arm)
- Modify: `crates/ritornello-core/src/status.rs` (`AudioOutputRequest`, `audio_output_put`, `AppState.audio_tx` type, tests)

**Interfaces:**
- Consumes: `AudioDevice` pairs from Task 1 (GET side untouched here).
- Produces: `Core::set_audio_device(&mut self, device: Option<String>) -> Result<()>` (None → `auto` to mpv, `audio_device = None`, persist); audio channel `mpsc::Sender<Option<String>>`; `PUT /api/audio-output` accepting `{"device": null}` (Task 3's UI relies on it).

- [ ] **Step 1: Write the failing tests**

In `core.rs` tests, update `set_audio_device_applique_et_persiste` to call `core.set_audio_device(Some("hw:CARD=Headphones".into()))` and add:

```rust
    #[tokio::test]
    async fn set_audio_device_none_revient_au_defaut_systeme() {
        // "System default" from the config page: nothing imposed on mpv
        // anymore (its native `auto`), and no device recorded on disk.
        let (mut core, player_calls, _sc, _rx, dir) = setup();
        core.set_audio_device(Some("hw:CARD=Headphones".into())).await.unwrap();
        core.set_audio_device(None).await.unwrap();
        assert!(player_calls.lock().unwrap().contains(&"audio_device auto".to_string()));
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.audio_device, None);
    }
```

In `status.rs` tests, add alongside the existing PUT tests:

```rust
    #[tokio::test]
    async fn put_audio_output_null_choisit_le_defaut_systeme() {
        let (state, mut audio_rx) = app_state_with_audio();
        let audio_current = state.audio_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/audio-output")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"device":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(audio_rx.recv().await.unwrap(), None);
        assert_eq!(*audio_current.read().await, None);
    }
```
Keep `put_audio_output_vide_renvoie_422_et_ne_change_rien` as is except its expected `audio_current` (still `Some("default")` — the 422 changes nothing) and adjust `put_audio_output_notifie_et_met_a_jour_la_selection_affichee`'s channel assertion to `Some("hw:CARD=Headphones".to_string())`.

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && cargo test -p ritornello-core audio 2>&1 | tail -5"
```
Expected: FAIL to compile (signature/type mismatches).

- [ ] **Step 3: Implement**

`core.rs` — `set_audio_device` becomes:

```rust
    /// Applies an output choice from the config page. `None` means "follow
    /// the system default": mpv gets its native `auto` back (settable at
    /// runtime), and nothing is recorded on disk — the same state as a fresh
    /// install, where `resume()` sends no device at all.
    pub async fn set_audio_device(&mut self, device: Option<String>) -> Result<()> {
        match &device {
            Some(d) => self.player.set_audio_device(d).await?,
            None => self.player.set_audio_device("auto").await?,
        }
        self.audio_device = device;
        self.persist();
        Ok(())
    }
```
(`resume()` is untouched: `audio_device: None` at boot keeps sending nothing.)

`main.rs` — the channel becomes `let (audio_tx, mut audio_rx) = mpsc::channel::<Option<String>>(4);` and the select arm:

```rust
            Some(device) = audio_rx.recv() => {
                if let Err(e) = core.set_audio_device(device).await {
                    tracing::warn!("changement de sortie audio: {e}");
                }
            }
```

`status.rs`:
```rust
#[derive(Deserialize)]
struct AudioOutputRequest {
    device: Option<String>,
}
```
`audio_output_put`: validate only when a name is given, then store/send the option:
```rust
async fn audio_output_put(State(state): State<AppState>, Json(req): Json<AudioOutputRequest>) -> Response {
    // `null` (or absent) = follow the system default. A named device is
    // validated as before: the empty string stays refused.
    if let Some(device) = &req.device {
        if let Err(msg) = validate_audio_device(device) {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
                .into_response();
        }
    }
    *state.audio_current.write().await = req.device.clone();
    if state.audio_tx.send(req.device).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
```
`AppState.audio_tx: mpsc::Sender<Option<String>>`; update the channel types in the `tests_support` constructors (`app_state`, `app_state_with_audio`, `app_state_with_cmd`, `app_state_fr` — and the fifth literal in `admin.rs` if the compiler flags it).

- [ ] **Step 4: Run the crate suite**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && cargo test -p ritornello-core 2>&1 | tail -5"
```
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add crates/ritornello-core/src
git commit -m "feat(core): device null sur /api/audio-output revient au defaut systeme"
```

---

### Task 3: The selector — System default entry, descriptions, robust current

**Files:**
- Modify: `web/app/src/types.ts`
- Modify: `web/app/src/views/ConfigView.vue`
- Modify: `web/app/src/views/ConfigView.test.ts`
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`

**Interfaces:**
- Consumes: GET pairs (Task 1), PUT nullable (Task 2).
- Produces: data attribute `data-audio-default` on the synthetic entry; i18n key `audio_default_device`.

- [ ] **Step 1: i18n keys and types**

`en.toml` (near `audio_output`): `audio_default_device = "System default"`.
`fr.toml`: `audio_default_device = "Par défaut (système)"`.

`types.ts`:
```ts
export interface AudioDevice { name: string; description: string }
export interface AudioPayload { devices: AudioDevice[]; current: string | null }
```

- [ ] **Step 2: Write the failing tests**

In `ConfigView.test.ts`: `charges()`'s `'/api/audio-output'` becomes
```ts
    '/api/audio-output': {
      devices: [
        { name: 'hw:CARD=Headphones', description: 'bcm2835 Headphones — Direct hardware device' },
        { name: 'hw:CARD=HDMI', description: '' },
      ],
      current: 'hw:CARD=HDMI',
    } as unknown,
```
Add `audio_default_device: 'Par défaut (système)'` to `CATALOGUE`. Then rewrite the « sortie audio » describe block:

```ts
describe('ConfigView — sortie audio', () => {
  beforeEach(reinitialiser)

  it('envoie le PUT du périphérique choisi, inchangé', async () => {
    const { w, puts } = await monter()
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=HDMI')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: 'hw:CARD=HDMI' } }])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('sans choix enregistré, l’entrée par défaut est sélectionnée et « Changer » envoie null', async () => {
    // Fini le repli sur le premier périphérique : `current: null` est un état
    // légitime (« suis le défaut système »), l'entrée synthétique le porte.
    const { w, puts } = await monter({
      '/api/audio-output': {
        devices: [{ name: 'hw:CARD=Headphones', description: '' }],
        current: null,
      },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('__system_default__')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: null } }])
  })

  it('l’entrée par défaut est la première de la liste', async () => {
    const { w } = await monter()
    const premier = w.findAllComponents(SelectItem)[0]!
    expect(premier.attributes('data-audio-default')).toBeDefined()
    expect(premier.text()).toBe('Par défaut (système)')
  })

  it('affiche la description en principal et le nom technique en secondaire', async () => {
    const { w } = await monter()
    const items = w.findAllComponents(SelectItem)
    const avecDescription = items.find((i) => i.text().includes('bcm2835 Headphones'))!
    expect(avecDescription.text()).toContain('hw:CARD=Headphones')
    // Sans description : le nom seul, pas de ligne secondaire vide.
    const sansDescription = items.find((i) => i.props('value') === 'hw:CARD=HDMI')!
    expect(sansDescription.text()).toBe('hw:CARD=HDMI')
  })

  it('un périphérique choisi mais absent de la liste reste visible', async () => {
    // Carte débranchée : la sélection courante est rajoutée en fin de liste
    // (nom seul) plutôt que de laisser un déclencheur vide.
    const { w } = await monter({
      '/api/audio-output': {
        devices: [{ name: 'hw:CARD=Headphones', description: '' }],
        current: 'hw:CARD=USB',
      },
    })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('hw:CARD=USB')
    const valeurs = w.findAllComponents(SelectItem).map((i) => i.props('value'))
    expect(valeurs).toContain('hw:CARD=USB')
  })

  it('aucun périphérique listé : l’entrée par défaut reste utilisable', async () => {
    const { w, puts } = await monter({ '/api/audio-output': { devices: [], current: null } })
    expect(w.findAllComponents(Select)[0]!.props('modelValue')).toBe('__system_default__')
    await w.find('[data-audio-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([{ url: '/api/audio-output', corps: { device: null } }])
  })
})
```
(The old tests « sans sortie choisie, retombe sur le premier périphérique » and « sans aucun périphérique listé, ne fabrique pas de nom » are REPLACED by the above — the fallback they pinned disappears with its reason. Keep their historical comments' spirit in the new comments.)

- [ ] **Step 3: Run tests to verify they fail**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 15
```
Expected: FAIL (`__system_default__` not selected, `data-audio-default` missing).

- [ ] **Step 4: Implement in `ConfigView.vue`**

Script — replace the fallback logic and `changerSortie`:

```ts
/**
 * Valeur de vue pour « Par défaut (système) » : jamais envoyée telle quelle
 * (« Changer » la traduit en `device: null`), et impossible à confondre avec
 * un nom de PCM ALSA.
 */
const DEFAUT_SYSTEME = '__system_default__'
```
In `chargerTout()`, the fallback line and its long comment become:
```ts
  // `current: null` = aucun choix enregistré : c'est l'entrée « Par défaut
  // (système) » qui le porte — plus de repli sur le premier périphérique
  // (c'était `null`, le PCM qui jette le son, en tête de `aplay -L`).
  device.value = audio.value.current ?? DEFAUT_SYSTEME
```
Add the computed for the robust list (near the `demarrage` computed):
```ts
// La sélection courante peut nommer un périphérique disparu (carte
// débranchée) : on la garde visible en fin de liste plutôt que de laisser
// le déclencheur vide.
const appareils = computed(() => {
  const liste = [...audio.value.devices]
  const courant = audio.value.current
  if (courant && !liste.some((d) => d.name === courant)) {
    liste.push({ name: courant, description: '' })
  }
  return liste
})
```
`changerSortie` becomes:
```ts
async function changerSortie() {
  const err = await api.put('/api/audio-output', {
    device: device.value === DEFAUT_SYSTEME ? null : device.value,
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}
```
Template — the audio card's `SelectContent` becomes:
```html
              <SelectContent>
                <SelectItem :value="DEFAUT_SYSTEME" data-audio-default>
                  {{ t('audio_default_device') }}
                </SelectItem>
                <!-- Description lisible en principal, nom technique en
                     secondaire — même motif que « Français » affiché / `fr`
                     envoyé pour les langues. -->
                <SelectItem v-for="d in appareils" :key="d.name" :value="d.name">
                  <div class="flex flex-col items-start">
                    <span>{{ d.description || d.name }}</span>
                    <span v-if="d.description" class="text-xs text-muted-foreground">{{ d.name }}</span>
                  </div>
                </SelectItem>
              </SelectContent>
```
Import `AudioDevice` type only if needed (the `AudioPayload` import already carries the shape).

- [ ] **Step 5: Run the web suite and typecheck**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 10
npm run typecheck -w app
```
Expected: all pass (including the `i18nKeysUsed` guard picking up `audio_default_device`).

- [ ] **Step 6: Commit**

```powershell
git add web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): entree Par defaut (systeme) et descriptions dans le selecteur audio"
```

---

### Task 4: Docs, full validation, e2e

**Files:**
- Modify: `docs/interface.md` (audio output paragraph)
- Check: `grep -in "audio" docs/*.md README.md` for stale descriptions of the selector

- [ ] **Step 1: Documentation**

In `docs/interface.md`, update the audio output description: entries show the device description with the technical name beneath; the first entry, "System default", means no device is imposed on mpv (the OS default applies, `audio-device=auto`) and is the state of a fresh install; the `null` ALSA device is not listed. Sweep other docs for stale selector descriptions (`docs/installation.md` mentions audio setup — check it still reads correctly; do not rename `/api/audio-output`).

- [ ] **Step 2: Full builds and test suites**

```powershell
npm run build --workspaces
npm run test --workspaces 2>&1 | Select-Object -Last 10
npm run typecheck -w app
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/selecteur-sortie-audio && touch crates/ritornello-core/build.rs && cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets 2>&1 | tail -5"
```
Expected: everything green.

- [ ] **Step 3: e2e**

```powershell
npm run e2e -w app 2>&1 | Select-Object -Last 15
```
Expected: 8/8 (the suite doesn't exercise the audio selector; this guards regressions on the config page as a whole). If chromium is missing: `npx playwright install chromium` first.

- [ ] **Step 4: Commit**

```powershell
git add docs
git commit -m "docs: selecteur de sortie audio avec descriptions et defaut systeme"
```
Integration (`git -C C:\projets\perso\ritornello merge --ff-only worktree-selecteur-sortie-audio`) happens after the final whole-branch review — controller's job, not this task's.
