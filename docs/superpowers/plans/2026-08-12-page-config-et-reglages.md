# Config Page and Behavior Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename the status page to config (with a sticky scrollspy TOC), add a `GET/PUT /api/settings` endpoint backing two new settings cards (startup power state, volume hold timings), and implement volume hold-to-repeat on both the web remote buttons and the physical remote.

**Architecture:** The core owns the settings (persisted in `state.json`, edited via `/api/settings`). The web remote does its own hold timers client-side. The `generic-input` plugin forwards kernel autorepeat events (`value == 2`) for volume-bound keys with a backward-compatible `"held": true` field on the input protocol line; the core paces held commands with one deadline field (no timer in the plugin, so a lost key-up cannot make the volume run away). Spec: `docs/superpowers/specs/2026-08-12-page-config-et-reglages-design.md`.

**Tech Stack:** Rust (axum, tokio, serde), Vue 3 + Vitest + Playwright, Tailwind.

## Global Constraints

- Build order is always **npm then cargo**: the SPA is embedded at compile time by `crates/ritornello-core/build.rs`.
- `cargo`/`clippy` run only through WSL: `wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test ..."`. `git`, `node`, `npm`, Playwright run on Windows. Multi-line or quote-heavy bash goes into a `.sh` file passed to `wsl.exe -- bash <path>` (inline strings get corrupted).
- Comments and documentation in English; diagnostic message strings, French locale pack values, and existing French identifiers stay French. Match each file's existing comment density and idiom.
- Settings JSON shape (wire and disk): `{"volume_repeat_initial_ms": 1000, "volume_repeat_interval_ms": 500, "start_in_standby": false}`. Bounds: initial 200–5000 ms, interval 100–2000 ms, 422 outside.
- Input protocol line: `{"cmd": "VolumeUp", "held": true}`; `held` absent = `false`; `held: false` is not serialized (wire stays byte-identical for existing messages).
- i18n keys added to BOTH `crates/ritornello-core/src/locales/en.toml` and `deploy/locales/core/fr.toml`. The `i18nKeysUsed` vitest guard fails the build if a used key is missing from the English catalog.
- No git push (no remote). Commit after every task. Never merge with a merge commit; integration to `main` at the end is `git merge --ff-only`.

---

### Task 0: Worktree build environment

The worktree `C:\projets\perso\ritornello\.claude\worktrees\config-page-et-reglages` is fresh: no `node_modules`, no `dist`, no `target`.

**Files:** none (environment only).

- [ ] **Step 1: Install and build the web workspaces (Windows)**

Run from the worktree root:
```powershell
npm ci
npm run build --workspaces
```
Expected: both `web/kit` and `web/app` build; `verifier-dist.mjs` passes.

- [ ] **Step 2: Prime the Rust build (WSL)**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test --workspace 2>&1 | tail -5"
```
Expected: full test suite passes (baseline green before any change).

---

### Task 1: `InputMessage` envelope in ritornello-proto

**Files:**
- Modify: `crates/ritornello-proto/src/command.rs`
- Modify: `crates/ritornello-proto/src/lib.rs` (re-export)

**Interfaces:**
- Produces: `ritornello_proto::InputMessage { cmd: Command, held: bool }`, `Serialize + Deserialize + Debug + Clone + PartialEq + Eq`. `From<Command>` for the non-held case.

- [ ] **Step 1: Write the failing tests** (in `mod tests` of `command.rs`)

```rust
    #[test]
    fn input_message_sans_held_est_une_commande_nue() {
        // Backward compatibility: an input plugin that writes a plain Command
        // line keeps working, and the non-held serialization is byte-identical.
        let msg: InputMessage = serde_json::from_str(r#"{"cmd":"VolumeUp"}"#).unwrap();
        assert_eq!(msg, InputMessage { cmd: Command::VolumeUp, held: false });
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"cmd":"VolumeUp"}"#);
    }

    #[test]
    fn input_message_held_roundtrip_avec_argument() {
        let msg: InputMessage = serde_json::from_str(r#"{"cmd":"Select","arg":3,"held":true}"#).unwrap();
        assert_eq!(msg, InputMessage { cmd: Command::Select(3), held: true });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<InputMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn input_message_from_command() {
        assert_eq!(InputMessage::from(Command::Stop), InputMessage { cmd: Command::Stop, held: false });
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-proto input_message 2>&1 | tail -5"
```
Expected: FAIL to compile (`InputMessage` not found).

- [ ] **Step 3: Implement** (in `command.rs`, after the `Command` enum)

```rust
/// One line of the input protocol: the command, plus whether it comes from a
/// **key being held down** (kernel autorepeat) rather than a fresh press.
///
/// `held` is additive and backward compatible: a plugin that writes a plain
/// `Command` line parses as `held: false`, and `held: false` is not
/// serialized, so existing messages stay byte-identical on the wire. The core
/// paces held volume commands itself (see `Core::handle_input`); `held` on any
/// other command is ignored there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMessage {
    #[serde(flatten)]
    pub cmd: Command,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

impl From<Command> for InputMessage {
    fn from(cmd: Command) -> Self {
        Self { cmd, held: false }
    }
}
```

In `lib.rs`, extend the existing `pub use` for the command module to also export `InputMessage` (match the file's existing re-export style, e.g. `pub use command::{Command, InputMessage};`).

- [ ] **Step 4: Run tests to verify they pass**

Same command as Step 2. Expected: 3 passed.

- [ ] **Step 5: Commit**

```powershell
git add crates/ritornello-proto
git commit -m "feat(proto): enveloppe InputMessage avec drapeau held retro-compatible"
```

---

### Task 2: `Settings` block in the persisted state

**Files:**
- Modify: `crates/ritornello-core/src/state.rs`

**Interfaces:**
- Produces: `crate::state::Settings { volume_repeat_initial_ms: u32, volume_repeat_interval_ms: u32, start_in_standby: bool }` with `Default` = 1000/500/false, `Clone + Debug + PartialEq + Eq + Serialize + Deserialize`; `PersistedState.settings: Settings` (serde-default).

- [ ] **Step 1: Write the failing tests** (in `mod tests` of `state.rs`)

```rust
    #[test]
    fn settings_par_defaut() {
        let s = Settings::default();
        assert_eq!(s.volume_repeat_initial_ms, 1000);
        assert_eq!(s.volume_repeat_interval_ms, 500);
        assert!(!s.start_in_standby);
        assert_eq!(PersistedState::default().settings, Settings::default());
    }

    #[test]
    fn un_state_json_sans_settings_reste_lisible() {
        // Backward compatibility: a state.json written before this version has
        // no `settings` block; it must load with the defaults.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"active_source":"radio","volume":42,"audio_device":null,"locale":"fr"}"#,
        )
        .unwrap();
        let st = load(&path);
        assert_eq!(st.settings, Settings::default());
        assert_eq!(st.volume, 42);
    }

    #[test]
    fn settings_roundtrip_et_bloc_partiel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut st = PersistedState::default();
        st.settings = Settings { volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, start_in_standby: true };
        save(&path, &st).unwrap();
        assert_eq!(load(&path), st);
        // A hand-edited partial block falls back to defaults for what's missing.
        std::fs::write(&path, r#"{"active_source":"radio","volume":42,"settings":{"start_in_standby":true}}"#).unwrap();
        let st = load(&path);
        assert!(st.settings.start_in_standby);
        assert_eq!(st.settings.volume_repeat_initial_ms, 1000);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-core state:: 2>&1 | tail -5"
```
Expected: FAIL to compile (`Settings` not found).

- [ ] **Step 3: Implement** (in `state.rs`)

```rust
/// Behavior settings, edited on the config page (`PUT /api/settings`).
/// Container-level `serde(default)`: a partial block in a hand-edited
/// state.json fills in with defaults instead of failing to load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Hold-to-repeat: delay before the first repeated volume step.
    pub volume_repeat_initial_ms: u32,
    /// Hold-to-repeat: delay between subsequent volume steps.
    pub volume_repeat_interval_ms: u32,
    /// Start in standby instead of waking the active source at launch.
    pub start_in_standby: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false }
    }
}
```

Add to `PersistedState` (after `mode`):
```rust
    /// Behavior settings (hold-to-repeat timings, startup power state).
    #[serde(default)]
    pub settings: Settings,
```
Update the `Default for PersistedState` impl to include `settings: Settings::default()`. The other test constructors in this file build `PersistedState` with struct literals — add `settings: Settings::default()` to each (or switch them to `..Default::default()` where that stays readable).

- [ ] **Step 4: Run tests to verify they pass**

Same command as Step 2. Expected: all `state::` tests pass.

- [ ] **Step 5: Commit**

```powershell
git add crates/ritornello-core/src/state.rs
git commit -m "feat(core): bloc settings persiste dans state.json"
```

---

### Task 3: Core — held-volume pacing, `set_settings`, start in standby

**Files:**
- Modify: `crates/ritornello-core/src/core.rs`

**Interfaces:**
- Consumes: `ritornello_proto::InputMessage` (Task 1), `crate::state::Settings` (Task 2).
- Produces: `Core::handle_input(&mut self, msg: InputMessage) -> Result<()>` (the new single entry point for commands), `Core::set_settings(&mut self, s: Settings)`, `Core::start_in_standby(&mut self) -> Result<()>`. `handle_command` stays public and unchanged in behavior.

**Design notes for the implementer:**
- `core.rs` uses `std::time::Instant` (already imported). Pacing tests use short real timings (core-side `set_settings` does not validate bounds — validation is the HTTP layer's job) plus `tokio::time::sleep`, like the overlay tests.
- One deadline field paces everything: a fresh (non-held) volume step arms `deadline = now + initial`; a held step is applied only when `now >= deadline` and re-arms `deadline = now + interval`.

- [ ] **Step 1: Write the failing tests** (in `mod tests` of `core.rs`; reuse the existing `setup()` helper which returns `(core, player_calls, source_calls, rx, dir)`)

```rust
    /// Short timings so pacing tests run in tens of milliseconds. The core does
    /// not validate bounds (that's the HTTP layer's job), so this is legal.
    fn reglages_rapides() -> crate::state::Settings {
        crate::state::Settings {
            volume_repeat_initial_ms: 30,
            volume_repeat_interval_ms: 25,
            start_in_standby: false,
        }
    }

    #[tokio::test]
    async fn volume_maintenu_est_ignore_avant_le_delai_initial() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 60 -> 65
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65, "une repetition avant le delai initial ne fait rien");
    }

    #[tokio::test]
    async fn volume_maintenu_repete_apres_le_delai_puis_a_lintervalle() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70, "premiere repetition apres le delai initial");
        // Immediately after: the interval has not elapsed yet.
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 70);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 75, "puis une par intervalle");
    }

    #[tokio::test]
    async fn volume_maintenu_sans_pression_initiale_ne_fait_rien() {
        // A held event with no prior press (core restarted mid-hold): no
        // deadline is armed, nothing moves.
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_input(InputMessage { cmd: Command::VolumeDown, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 60);
    }

    #[tokio::test]
    async fn held_sur_une_commande_non_volume_est_ignore() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        source_calls.lock().unwrap().clear();
        core.handle_input(InputMessage { cmd: Command::Next, held: true }).await.unwrap();
        assert!(source_calls.lock().unwrap().is_empty(), "un Next maintenu ne doit pas atteindre la source");
    }

    #[tokio::test]
    async fn volume_maintenu_est_bloque_en_veille() {
        let (mut core, _pc, _sc, _rx, _d) = setup();
        core.set_settings(reglages_rapides());
        core.resume().await.unwrap();
        core.handle_command(Command::VolumeUp).await.unwrap(); // 65, arms the deadline
        core.handle_command(Command::Power).await.unwrap();    // standby
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        core.handle_input(InputMessage { cmd: Command::VolumeUp, held: true }).await.unwrap();
        assert_eq!(core.etat_lecteur().volume, 65);
    }

    #[tokio::test]
    async fn handle_input_non_held_equivaut_a_handle_command() {
        let (mut core, _pc, source_calls, _rx, _d) = setup();
        core.resume().await.unwrap();
        core.handle_input(InputMessage::from(Command::Select(3))).await.unwrap();
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Select(3)")));
    }

    #[tokio::test]
    async fn set_settings_persiste() {
        let (mut core, _pc, _sc, _rx, dir) = setup();
        core.set_settings(crate::state::Settings {
            volume_repeat_initial_ms: 800,
            volume_repeat_interval_ms: 250,
            start_in_standby: true,
        });
        let st = crate::state::load(&dir.path().join("state.json"));
        assert_eq!(st.settings.volume_repeat_initial_ms, 800);
        assert!(st.settings.start_in_standby);
    }

    #[tokio::test]
    async fn demarrage_en_veille_applique_le_volume_sans_reveiller_la_source() {
        let (mut core, player_calls, source_calls, mut rx, _d) = setup();
        core.start_in_standby().await.unwrap();
        // mpv is configured (volume applied) so waking later starts right...
        assert!(player_calls.lock().unwrap().iter().any(|c| c.starts_with("volume")));
        // ...but the source was NOT woken, and the display shows standby.
        assert!(!source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")), "pas de Wake en veille");
        assert_eq!(rx.borrow_and_update().line1, "STANDBY");
        assert!(core.etat_lecteur().standby);
        // Power then wakes normally.
        core.handle_command(Command::Power).await.unwrap();
        assert!(!core.etat_lecteur().standby);
        assert!(source_calls.lock().unwrap().iter().any(|c| c.contains("Wake")));
    }
```

Adjust the exact `source_calls` string patterns to what `FakeSource` records (see the existing tests around `set_locale_persiste_et_notifie_les_sources` — the format is `"<name>:<Req>(..)"` or similar; read `FakeSource::request` in the test module first and match its exact format). Also check `etat_lecteur()` visibility: if it is private, assert through the existing `etat_rx` receiver pattern used by `standby_bloque_tout_sauf_power` instead.

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-core volume_maintenu 2>&1 | tail -5"
```
Expected: FAIL to compile (`handle_input`, `set_settings`, `start_in_standby` not found).

- [ ] **Step 3: Implement**

In the imports, add `InputMessage` to the existing `ritornello_proto` use, and `Duration` if not already imported. Add two fields to `Core`:

```rust
    /// Behavior settings (hold-to-repeat timings, startup power state),
    /// persisted with the rest of the state.
    settings: crate::state::Settings,
    /// Hold-to-repeat pacing: instant before which a held volume command is
    /// ignored. Armed by a fresh volume step (now + initial delay), re-armed
    /// by each applied repeat (now + interval). `None` until a first press —
    /// a held event arriving out of nowhere (core restarted mid-hold) does
    /// nothing.
    volume_deadline: Option<Instant>,
```

In `Core::new`, initialize `settings: persisted.settings.clone()` and `volume_deadline: None`.

Extract the volume step from `appliquer_commande` (the body of the `VolumeUp | VolumeDown` arm) into:

```rust
    /// One volume step (±5), applied to mpv, persisted, shown as an overlay.
    /// Shared by fresh presses and held repeats; only the caller decides how
    /// to re-arm `volume_deadline`.
    async fn step_volume(&mut self, up: bool) -> Result<()> {
        let v = self.volume as i16 + if up { 5 } else { -5 };
        self.volume = v.clamp(0, 100) as u8;
        self.player.set_volume(self.volume).await?;
        self.persist();
        self.show_overlay().await;
        Ok(())
    }
```

The `VolumeUp | VolumeDown` arm becomes:

```rust
            Command::VolumeUp | Command::VolumeDown => {
                self.step_volume(cmd == Command::VolumeUp).await?;
                self.volume_deadline = Some(
                    Instant::now() + Duration::from_millis(self.settings.volume_repeat_initial_ms.into()),
                );
            }
```

Add the public methods:

```rust
    /// Entry point for everything that used to call `handle_command`: fresh
    /// commands pass through unchanged; held (autorepeat) volume commands are
    /// paced by `volume_deadline`. Held on any other command is a no-op — the
    /// remote's autorepeat only means something for the volume.
    pub async fn handle_input(&mut self, msg: InputMessage) -> Result<()> {
        if !msg.held {
            return self.handle_command(msg.cmd).await;
        }
        if self.standby {
            return Ok(());
        }
        let up = match msg.cmd {
            Command::VolumeUp => true,
            Command::VolumeDown => false,
            _ => return Ok(()),
        };
        let Some(deadline) = self.volume_deadline else { return Ok(()) };
        if Instant::now() < deadline {
            return Ok(());
        }
        let issue = self.step_volume(up).await;
        self.volume_deadline =
            Some(Instant::now() + Duration::from_millis(self.settings.volume_repeat_interval_ms.into()));
        // Same publication contract as `handle_command`: the UI must see the
        // new volume even if mpv errored mid-way.
        self.publie_etat();
        issue
    }

    /// New settings from `PUT /api/settings` (via the `select!` loop of main).
    /// No bounds check here: the HTTP layer validates, and tests rely on tiny
    /// timings.
    pub fn set_settings(&mut self, s: crate::state::Settings) {
        self.settings = s;
        self.persist();
    }

    /// Startup in standby (`settings.start_in_standby`): mpv is configured
    /// (volume, audio device) so a later wake starts right, but the active
    /// source is not woken and the display shows the standby view.
    pub async fn start_in_standby(&mut self) -> Result<()> {
        self.standby = true;
        self.player.set_volume(self.volume).await?;
        if let Some(device) = self.audio_device.clone() {
            self.player.set_audio_device(&device).await?;
        }
        self.view = self.standby_view().await;
        self.push_view();
        self.publie_etat();
        Ok(())
    }
```

In `persist()`, add `settings: self.settings.clone()` to the `PersistedState` literal.

- [ ] **Step 4: Run the core test suite**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-core 2>&1 | tail -5"
```
Expected: all pass (including the pre-existing overlay and standby tests).

- [ ] **Step 5: Commit**

```powershell
git add crates/ritornello-core/src/core.rs
git commit -m "feat(core): cadencement du volume maintenu et demarrage en veille"
```

---

### Task 4: `GET/PUT /api/settings` + wiring and startup branch in main

**Files:**
- Modify: `crates/ritornello-core/src/status.rs`
- Modify: `crates/ritornello-core/src/main.rs`

**Interfaces:**
- Consumes: `state::Settings` (Task 2), `Core::set_settings` / `Core::start_in_standby` (Task 3).
- Produces: `GET /api/settings` → `Settings` JSON; `PUT /api/settings` → 204 / 422 `{"error": "..."}`; `pub fn validate_settings(&Settings) -> Result<(), String>`; `AppState.settings_current: Arc<RwLock<Settings>>` and `AppState.settings_tx: mpsc::Sender<Settings>`.

- [ ] **Step 1: Write the failing tests** (in `mod tests` of `status.rs`)

```rust
    /// Variant with an observable `settings_tx`, for the `/api/settings` tests.
    fn app_state_with_settings() -> (AppState, tokio::sync::mpsc::Receiver<crate::state::Settings>) {
        let (state, _audio_rx) = app_state_with_audio();
        let (settings_tx, settings_rx) = tokio::sync::mpsc::channel(4);
        (AppState { settings_tx, ..state }, settings_rx)
    }

    #[tokio::test]
    async fn get_settings_renvoie_les_valeurs_courantes() {
        let app = router(app_state());
        let resp = app.oneshot(Request::get("/api/settings").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["volume_repeat_initial_ms"], 1000);
        assert_eq!(v["volume_repeat_interval_ms"], 500);
        assert_eq!(v["start_in_standby"], false);
    }

    #[tokio::test]
    async fn put_settings_notifie_et_met_a_jour_la_selection() {
        let (state, mut settings_rx) = app_state_with_settings();
        let settings_current = state.settings_current.clone();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::put("/api/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"volume_repeat_initial_ms":800,"volume_repeat_interval_ms":250,"start_in_standby":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let recu = settings_rx.recv().await.unwrap();
        assert_eq!(recu.volume_repeat_initial_ms, 800);
        assert!(recu.start_in_standby);
        assert_eq!(settings_current.read().await.volume_repeat_interval_ms, 250);
    }

    #[tokio::test]
    async fn put_settings_hors_bornes_renvoie_422_et_ne_change_rien() {
        // Same contract as /api/audio-output and /api/theme: validated before
        // any state change, with an `error` message the SPA turns into a toast.
        let (state, mut settings_rx) = app_state_with_settings();
        let settings_current = state.settings_current.clone();
        let app = router(state);
        for corps in [
            r#"{"volume_repeat_initial_ms":100,"volume_repeat_interval_ms":500,"start_in_standby":false}"#,
            r#"{"volume_repeat_initial_ms":1000,"volume_repeat_interval_ms":50,"start_in_standby":false}"#,
            r#"{"volume_repeat_initial_ms":9000,"volume_repeat_interval_ms":500,"start_in_standby":false}"#,
        ] {
            // `AppState` est `Clone` : chaque oneshot repart du même montage.
            let resp = app
                .clone()
                .oneshot(
                    Request::put("/api/settings")
                        .header("content-type", "application/json")
                        .body(Body::from(corps))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "{corps}");
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(v["error"].is_string());
        }
        assert_eq!(settings_current.read().await.volume_repeat_initial_ms, 1000);
        assert!(settings_rx.try_recv().is_err(), "rien ne doit partir dans le canal");
    }

    #[test]
    fn validate_settings_borne_les_deux_delais() {
        use crate::state::Settings;
        assert!(validate_settings(&Settings::default()).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 200, volume_repeat_interval_ms: 100, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 5000, volume_repeat_interval_ms: 2000, ..Default::default() }).is_ok());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 199, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_initial_ms: 5001, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_interval_ms: 99, ..Default::default() }).is_err());
        assert!(validate_settings(&Settings { volume_repeat_interval_ms: 2001, ..Default::default() }).is_err());
    }
```

(If the `put_settings_hors_bornes` loop over fresh states reads awkwardly once in the file, simplify to three sequential oneshots on clones of the same `router(state.clone())` — `AppState` is `Clone`. Keep the invariants: 422, `error` string, `settings_current` untouched, channel empty.)

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-core settings 2>&1 | tail -5"
```
Expected: FAIL to compile (missing `AppState` fields, `validate_settings`).

- [ ] **Step 3: Implement in `status.rs`**

Add to `AppState`:
```rust
    /// Behavior settings shown on the config page. Same pattern as
    /// `theme_current`/`theme_tx`: the HTTP layer validates and updates the
    /// shared copy, the channel carries the change to the core loop.
    pub settings_current: Arc<RwLock<crate::state::Settings>>,
    pub settings_tx: mpsc::Sender<crate::state::Settings>,
```

Route: `.route("/api/settings", get(settings_json).put(settings_put))`.

Handlers + validation (place near `validate_audio_device`):
```rust
/// Bounds for the hold-to-repeat timings. Pure function, same model as
/// `validate_audio_device`: the core itself accepts anything (tests use tiny
/// timings), the HTTP surface is where user input is checked.
pub fn validate_settings(s: &crate::state::Settings) -> Result<(), String> {
    if !(200..=5000).contains(&s.volume_repeat_initial_ms) {
        return Err("délai initial hors bornes (200-5000 ms)".to_string());
    }
    if !(100..=2000).contains(&s.volume_repeat_interval_ms) {
        return Err("intervalle de répétition hors bornes (100-2000 ms)".to_string());
    }
    Ok(())
}

async fn settings_json(State(state): State<AppState>) -> Json<crate::state::Settings> {
    Json(state.settings_current.read().await.clone())
}

/// Full replacement: the SPA GETs the struct, edits it, and PUTs it back
/// whole. A field absent from the body falls back to its default (the struct
/// is `serde(default)`), which is the price of reusing the state type — fine
/// on a single-user device.
async fn settings_put(State(state): State<AppState>, Json(req): Json<crate::state::Settings>) -> Response {
    if let Err(msg) = validate_settings(&req) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    *state.settings_current.write().await = req.clone();
    if state.settings_tx.send(req).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}
```

Add the two new fields to EVERY `AppState` literal in `tests_support` (`app_state`, `app_state_with_audio`, `app_state_with_cmd`, `app_state_fr`):
```rust
            settings_current: Arc::new(tokio::sync::RwLock::new(crate::state::Settings::default())),
            settings_tx: tokio::sync::mpsc::channel(4).0,
```

- [ ] **Step 4: Wire in `main.rs`**

After the `theme_tx` channel creation (line ~101):
```rust
    let (settings_tx, mut settings_rx) = mpsc::channel::<state::Settings>(4);
```
Near `theme_current` (line ~304):
```rust
    let settings_current = Arc::new(RwLock::new(persisted.settings.clone()));
```
In the `AppState` literal: `settings_current: settings_current.clone(), settings_tx: settings_tx.clone(),`.

Before `core::Core::new` (where `persisted` is moved), capture the flag:
```rust
    let start_in_standby = persisted.settings.start_in_standby;
```
Replace the `core.resume()` call (line ~356) with:
```rust
    // Best-effort, like the wake via `Power` (see the comment below): startup
    // must never put systemd in a restart loop. `start_in_standby` skips the
    // source wake but still configures mpv, so the first `Power` starts right.
    let demarrage = if start_in_standby { core.start_in_standby().await } else { core.resume().await };
    if let Err(e) = demarrage {
        tracing::warn!("reveil au demarrage: {e}");
    }
```
(Fold the existing best-effort comment into this block rather than duplicating it.)

Add a `select!` arm after the `theme_rx` arm:
```rust
            Some(s) = settings_rx.recv() => {
                core.set_settings(s);
            }
```

- [ ] **Step 5: Run the crate test suite**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-core 2>&1 | tail -5"
```
Expected: all pass.

- [ ] **Step 6: Commit**

```powershell
git add crates/ritornello-core/src/status.rs crates/ritornello-core/src/main.rs
git commit -m "feat(core): reglages exposes par GET/PUT /api/settings et appliques au demarrage"
```

---

### Task 5: Input protocol carries `InputMessage` end to end

**Files:**
- Modify: `crates/ritornello-plugin-sdk/src/client.rs` (`run_input_client`)
- Modify: `crates/ritornello-plugin-sdk/src/server.rs` (`InputPlugin`, `run_input_plugin`)
- Modify: `crates/ritornello-core/src/main.rs` (channel type, `handle_input`)
- Modify: `crates/ritornello-core/src/status.rs` (`command_post`, `AppState.cmd_tx` type, tests)
- Modify: `crates/ritornello-plugin-generic-input/src/main.rs` (compile fix only: `EvdevInput` channel type — behavior change comes in Task 6)
- Modify: `crates/ritornello-plugin-generic-input/src/devices.rs` (compile fix only: `Hub.tx` type, wrap sends in `InputMessage::from`)

**Interfaces:**
- Consumes: `InputMessage` (Task 1), `Core::handle_input` (Task 3).
- Produces: `run_input_client(socket_path: &Path, cmd_tx: mpsc::Sender<InputMessage>)`; `trait InputPlugin { async fn next_command(&mut self) -> Result<InputMessage> }`; `AppState.cmd_tx: mpsc::Sender<InputMessage>`; `POST /api/command` accepts both the bare `Command` shape and the envelope.

**Note:** the wire format is unchanged for existing messages (Task 1 guarantees it); this task only changes Rust types. The workspace does not compile until all six files are updated — do the edits together, then run the tests.

- [ ] **Step 1: Write the failing tests**

In `client.rs` tests (follow the style of the existing socket tests there — bind a `UnixListener`, connect the client, write lines):
```rust
    #[tokio::test]
    async fn input_client_relaie_les_lignes_avec_et_sans_held() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("input.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let socket_for_client = socket.clone();
        tokio::spawn(async move {
            let _ = run_input_client(&socket_for_client, tx).await;
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        // A plain line from a pre-envelope plugin, then a held line.
        stream.write_all(b"{\"cmd\":\"VolumeUp\"}\n{\"cmd\":\"VolumeDown\",\"held\":true}\n").await.unwrap();
        let premier = rx.recv().await.unwrap();
        assert_eq!(premier, ritornello_proto::InputMessage::from(ritornello_proto::Command::VolumeUp));
        let second = rx.recv().await.unwrap();
        assert_eq!(second.cmd, ritornello_proto::Command::VolumeDown);
        assert!(second.held);
    }
```
In `server.rs` `input_tests`, update `FixedCommands` to return `InputMessage` values and assert that a held message serializes with `"held":true` on the wire while a non-held one has no `held` key.

In `status.rs` tests, update the two `post_command_*` tests to assert on `.cmd` (`cmd_rx.recv().await.unwrap().cmd`) and add:
```rust
    #[tokio::test]
    async fn post_command_accepte_le_drapeau_held() {
        let (state, mut cmd_rx) = app_state_with_cmd();
        let app = router(state);
        let resp = app
            .oneshot(
                Request::post("/api/command")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"cmd":"VolumeUp","held":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let recu = cmd_rx.recv().await.unwrap();
        assert_eq!(recu.cmd, ritornello_proto::Command::VolumeUp);
        assert!(recu.held);
    }
```

- [ ] **Step 2: Implement across the six files**

`client.rs`: change the signature to `pub async fn run_input_client(socket_path: &Path, cmd_tx: mpsc::Sender<InputMessage>) -> Result<()>` and the parse type to `InputMessage` (import from `ritornello_proto`). Error message string unchanged.

`server.rs`: `trait InputPlugin { async fn next_command(&mut self) -> Result<InputMessage>; }`; `run_input_plugin` serializes the `InputMessage` (serde skips `held: false`, keeping old wire bytes).

`main.rs` (core): `let (cmd_tx, mut cmd_rx) = mpsc::channel::<InputMessage>(32);` (import `InputMessage` in the `ritornello_proto` use). The `select!` arm becomes:
```rust
            Some(msg) = cmd_rx.recv() => {
                if let Err(e) = core.handle_input(msg).await {
                    tracing::warn!("commande: {e}");
                }
                status_state.write().await.active_source = core.active_source().to_string();
            }
```

`status.rs`: `pub cmd_tx: mpsc::Sender<ritornello_proto::InputMessage>`; `command_post` takes `Json(msg): Json<ritornello_proto::InputMessage>` and sends `msg` (the doc comment gains one line: the envelope's `held` flag passes through, so the core paces held volume commands wherever they come from). Update `app_state_with_cmd`'s channel type.

`generic-input` (compile fix only, no behavior change yet): `Hub.tx: mpsc::Sender<InputMessage>`, `Hub::new` accordingly; in `spawn_reader` send `InputMessage::from(cmd)`; `EvdevInput { rx: mpsc::Receiver<InputMessage> }` and its `next_command` returns the received message; in `main.rs` (plugin) and tests, update channel types (`mpsc::channel::<InputMessage>(...)`).

- [ ] **Step 3: Run the workspace test suite**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test --workspace 2>&1 | tail -5"
```
Expected: all pass.

- [ ] **Step 4: Commit**

```powershell
git add crates/ritornello-plugin-sdk crates/ritornello-core crates/ritornello-plugin-generic-input
git commit -m "feat(sdk): le protocole input transporte InputMessage de bout en bout"
```

---

### Task 6: generic-input forwards autorepeat for volume keys

**Files:**
- Modify: `crates/ritornello-plugin-generic-input/src/devices.rs`

**Interfaces:**
- Consumes: `InputMessage` (Task 1), `key_outcome` (existing pure function, unchanged).
- Produces: `pub fn key_outcome_held(bindings: &Bindings, learning_device: Option<&str>, device_name: &str, code: u16, held: bool) -> Option<InputMessage>`.

- [ ] **Step 1: Write the failing tests** (in `mod tests` of `devices.rs`; the `table()` helper binds code 115 → `VolumeUp` on "eHome")

```rust
    #[test]
    fn key_outcome_held_marque_les_repetitions_du_volume() {
        let t = table();
        let presse = key_outcome_held(&t, None, "eHome", 115, false).unwrap();
        assert_eq!(presse, InputMessage::from(Command::VolumeUp));
        let repete = key_outcome_held(&t, None, "eHome", 115, true).unwrap();
        assert_eq!(repete.cmd, Command::VolumeUp);
        assert!(repete.held);
    }

    #[test]
    fn key_outcome_held_ignore_les_repetitions_hors_volume() {
        // Holding Stop or Next must not machine-gun the command: autorepeat
        // only means something for the volume.
        let mut t = table();
        t.devices[0].bindings.push(Binding::new(166, &Command::Stop));
        assert_eq!(key_outcome_held(&t, None, "eHome", 166, true), None);
        // The fresh press still goes through.
        assert!(key_outcome_held(&t, None, "eHome", 166, false).is_some());
    }

    #[test]
    fn key_outcome_held_respecte_lapprentissage() {
        let t = table();
        assert_eq!(key_outcome_held(&t, Some("eHome"), "eHome", 115, true), None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-plugin-generic-input key_outcome_held 2>&1 | tail -5"
```
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

Import `InputMessage` alongside `Command`. Add below `key_outcome`:

```rust
/// Same resolution as `key_outcome`, plus the autorepeat rule: a held key
/// (evdev `value == 2`) only emits for the volume commands, marked `held` so
/// the core paces them (the kernel repeats much faster than one step per
/// 500 ms should go). Pure function, testable without hardware.
pub fn key_outcome_held(
    bindings: &Bindings,
    learning_device: Option<&str>,
    device_name: &str,
    code: u16,
    held: bool,
) -> Option<InputMessage> {
    let cmd = key_outcome(bindings, learning_device, device_name, code)?;
    if held && !matches!(cmd, Command::VolumeUp | Command::VolumeDown) {
        return None;
    }
    Some(InputMessage { cmd, held })
}
```

In `spawn_reader`, replace the filter and emission (currently `value() != 1` → `key_outcome` → send):

```rust
                let value = ev.value();
                // 1 = key down, 2 = kernel autorepeat while held. Release (0)
                // stays ignored: the core paces repeats, no timer to stop here.
                if ev.event_type() != EventType::KEY || (value != 1 && value != 2) {
                    continue;
                }
                if value == 1 {
                    // Learning consumes the first press and emits nothing.
                    let capture = { hub.learn.write().unwrap().capture(&name, ev.code()) };
                    if capture {
                        continue;
                    }
                }
                // No lock guard crosses the send `.await`.
                let msg = {
                    let learn = hub.learn.read().unwrap();
                    let b = hub.bindings.read().unwrap();
                    key_outcome_held(&b, learn.device(), &name, ev.code(), value == 2)
                };
                if let Some(msg) = msg {
                    tracing::debug!("{name}: touche {} -> {:?}", ev.code(), msg.cmd);
                    let _ = hub.tx.send(msg).await;
                }
```

- [ ] **Step 4: Run the plugin test suite**

```powershell
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && cargo test -p ritornello-plugin-generic-input 2>&1 | tail -5"
```
Expected: all pass.

- [ ] **Step 5: Commit**

```powershell
git add crates/ritornello-plugin-generic-input
git commit -m "feat(generic-input): les repetitions noyau des touches volume partent marquees held"
```

---

### Task 7: Rename the status page to config (route, view, nav, i18n)

**Files:**
- Rename: `web/app/src/views/StatusView.vue` → `web/app/src/views/ConfigView.vue` (git mv)
- Rename: `web/app/src/views/StatusView.test.ts` → `web/app/src/views/ConfigView.test.ts` (git mv)
- Modify: `web/app/src/router.ts`, `web/app/src/router.test.ts`, `web/app/src/App.vue`
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`
- Modify: `web/app/e2e/parcours.spec.ts` (URL only; the run happens in Task 11)

**Interfaces:**
- Produces: route `/config` (name `config`), redirect `/status` → `/config`; i18n keys `config_title` ("configuration"/"configuration") and `plugins_title` ("Plugins"/"Plugins"); `status_title` removed from both catalogs.

- [ ] **Step 1: git mv and failing tests**

```powershell
git mv web/app/src/views/StatusView.vue web/app/src/views/ConfigView.vue
git mv web/app/src/views/StatusView.test.ts web/app/src/views/ConfigView.test.ts
```

In `router.test.ts`, update the first test (`/status` expectation becomes name `config` after redirect) and add the redirect check:
```ts
  it('conserve les URL historiques', async () => {
    await router.push('/')
    expect(router.currentRoute.value.name).toBe('home')
    await router.push('/config')
    expect(router.currentRoute.value.name).toBe('config')
    await router.push('/plugins/radio/')
    // ... (rest unchanged)
  })

  it("redirige l'ancienne URL /status vers /config", async () => {
    // The page was renamed (it configures more than it reports), but /status
    // stayed a valid URL since the server-rendered days: it now lands on the
    // same page under its new name.
    await router.push('/status')
    expect(router.currentRoute.value.fullPath).toBe('/config')
    expect(router.currentRoute.value.name).toBe('config')
  })
```

In `ConfigView.test.ts`: import stays `./ConfigView.vue`; in `CATALOGUE`, replace `status_title: 'Statut'` with `config_title: 'Configuration'` and add `plugins_title: 'Plugins'`; in `monter()`, the memory route `/status` becomes `/config`; the empty-table assertion `expect(w.text()).toContain('Statut')` becomes `toContain('Plugins')`; rename the `describe` prefixes from `StatusView` to `ConfigView`.

- [ ] **Step 2: Run tests to verify they fail**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 15
```
Expected: FAIL (router has no `/config`, view still calls `t('status_title')`).

- [ ] **Step 3: Implement**

`router.ts` — replace the `/status` route (keep the historical-URL comment, amended):
```ts
    // `/status` est l'URL historique de cette page (servie depuis les débuts
    // par le cœur) : elle reste valide et redirige vers son nouveau nom.
    { path: '/config', name: 'config', component: () => import('./views/ConfigView.vue') },
    { path: '/status', redirect: '/config' },
```

`App.vue` — nav link:
```html
        <RouterLink to="/config" class="text-sm text-muted-foreground">{{ t('config_title') }}</RouterLink>
```

`ConfigView.vue` — the first card's title becomes `{{ t('plugins_title') }}` (the page title is the nav entry; the card lists plugin status, which deserves its own label now that the page is about configuration).

`crates/ritornello-core/src/locales/en.toml` — replace `status_title = "status"` with:
```toml
config_title = "configuration"
plugins_title = "Plugins"
```
`deploy/locales/core/fr.toml` — replace `status_title = "statut"` with:
```toml
config_title = "configuration"
plugins_title = "Plugins"
```

`web/app/e2e/parcours.spec.ts` — `await page.goto('/status')` becomes `await page.goto('/config')` and the test title `le statut` becomes `la config`.

- [ ] **Step 4: Run the web tests**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 10
```
Expected: all pass, including `i18nKeysUsed` (proof `config_title`/`plugins_title` exist in the embedded English catalog and nothing references `status_title` anymore).

- [ ] **Step 5: Commit**

```powershell
git add -A web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml web/app/e2e/parcours.spec.ts
git commit -m "feat(web): la page statut devient la page configuration"
```

---

### Task 8: Settings cards (startup, volume hold) on the config page

**Files:**
- Modify: `web/app/src/views/ConfigView.vue`
- Modify: `web/app/src/views/ConfigView.test.ts`
- Modify: `web/app/src/types.ts`
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml`

**Interfaces:**
- Consumes: `GET/PUT /api/settings` (Task 4).
- Produces: `SettingsPayload` in `types.ts`; data attributes `data-startup-select`, `data-startup-change`, `data-hold-initial`, `data-hold-interval`, `data-hold-change` (Tasks 9 and 11 rely on the cards existing).

- [ ] **Step 1: i18n keys**

`en.toml` (after `recent_errors`):
```toml
startup_title = "Startup"
startup_on = "on"
startup_standby = "standby"
volume_hold_title = "Volume hold"
volume_hold_initial = "Initial delay (ms)"
volume_hold_interval = "Repeat interval (ms)"
```
`fr.toml`:
```toml
startup_title = "Démarrage"
startup_on = "allumé"
startup_standby = "veille"
volume_hold_title = "Volume maintenu"
volume_hold_initial = "Délai initial (ms)"
volume_hold_interval = "Intervalle de répétition (ms)"
```

`types.ts`:
```ts
/** Réglages de comportement, tels que les sert `GET /api/settings`. */
export interface SettingsPayload {
  volume_repeat_initial_ms: number
  volume_repeat_interval_ms: number
  start_in_standby: boolean
}
```

- [ ] **Step 2: Write the failing tests** (in `ConfigView.test.ts`)

Add `'/api/settings': { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false }` to `charges()`, and the keys `startup_title: 'Démarrage', startup_on: 'allumé', startup_standby: 'veille', volume_hold_title: 'Volume maintenu', volume_hold_initial: 'Délai initial (ms)', volume_hold_interval: 'Intervalle de répétition (ms)'` to `CATALOGUE`.

```ts
describe('ConfigView — réglages', () => {
  beforeEach(reinitialiser)

  it('affiche les réglages lus depuis /api/settings', async () => {
    const { w } = await monter({
      '/api/settings': { volume_repeat_initial_ms: 800, volume_repeat_interval_ms: 250, start_in_standby: true },
    })
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('800')
    expect((w.find('[data-hold-interval]').element as HTMLInputElement).value).toBe('250')
    // Le sélecteur de démarrage reflète la veille.
    const demarrage = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'standby')
    expect(demarrage).toBeDefined()
  })

  it('enregistre le démarrage en veille par un PUT du bloc complet', async () => {
    const { w, puts } = await monter()
    const demarrage = w.findAllComponents(Select).find((s) => s.props('modelValue') === 'on')!
    await demarrage.vm.$emit('update:modelValue', 'standby')
    await w.find('[data-startup-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        corps: { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: true },
      },
    ])
    expect(toast.success).toHaveBeenCalledWith('OK')
  })

  it('enregistre les délais du volume maintenu en nombres', async () => {
    const { w, puts } = await monter()
    await w.find('[data-hold-initial]').setValue('1500')
    await w.find('[data-hold-interval]').setValue('300')
    await w.find('[data-hold-change]').trigger('click')
    await flushPromises()
    expect(puts).toEqual([
      {
        url: '/api/settings',
        corps: { volume_repeat_initial_ms: 1500, volume_repeat_interval_ms: 300, start_in_standby: false },
      },
    ])
  })

  it('un PUT de réglages refusé est signalé par un toast', async () => {
    const { w } = await monter({}, 'délai initial hors bornes (200-5000 ms)')
    await w.find('[data-hold-change]').trigger('click')
    await flushPromises()
    expect(toast.error).toHaveBeenCalledWith('délai initial hors bornes (200-5000 ms)')
  })

  it('un /api/settings injoignable laisse les valeurs par défaut', async () => {
    const { w } = await monter({ '/api/settings': undefined })
    expect((w.find('[data-hold-initial]').element as HTMLInputElement).value).toBe('1000')
  })
})
```

(If the kit `Select` cannot be told apart by `modelValue` because the audio select shares a value, give the startup trigger `:aria-label="t('startup_title')"` and find by that instead — mirror whichever selector proves stable.)

- [ ] **Step 3: Run tests to verify they fail**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 15
```
Expected: FAIL (`data-hold-initial` not found).

- [ ] **Step 4: Implement in `ConfigView.vue`**

Script: import `Input` from `@ritornello/ui` (it is exported — the theme picker uses it), add:
```ts
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 1000,
  volume_repeat_interval_ms: 500,
  start_in_standby: false,
})
// Le Select ne porte que des chaînes : « on »/« standby » traduits à
// l'affichage, le booléen reste la valeur envoyée au cœur.
const demarrage = computed({
  get: () => (reglages.value.start_in_standby ? 'standby' : 'on'),
  set: (v: string) => { reglages.value.start_in_standby = v === 'standby' },
})

async function enregistrerReglages() {
  const err = await api.put('/api/settings', {
    ...reglages.value,
    volume_repeat_initial_ms: Number(reglages.value.volume_repeat_initial_ms),
    volume_repeat_interval_ms: Number(reglages.value.volume_repeat_interval_ms),
  })
  toast[err ? 'error' : 'success'](err ?? t.value('ok'))
}
```
In `chargerTout()` add:
```ts
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
```
Import `computed` from `vue` and `SettingsPayload` from `../types`.

Template — two cards between the language card and the logs card:
```html
    <Card>
      <CardHeader><CardTitle>{{ t('startup_title') }}</CardTitle></CardHeader>
      <CardContent class="flex flex-wrap items-center gap-2">
        <Select v-model="demarrage">
          <SelectTrigger class="min-w-32" data-startup-select :aria-label="t('startup_title')"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="on">{{ t('startup_on') }}</SelectItem>
            <SelectItem value="standby">{{ t('startup_standby') }}</SelectItem>
          </SelectContent>
        </Select>
        <Button data-startup-change @click="enregistrerReglages">{{ t('change') }}</Button>
      </CardContent>
    </Card>

    <Card>
      <CardHeader><CardTitle>{{ t('volume_hold_title') }}</CardTitle></CardHeader>
      <CardContent class="flex flex-wrap items-end gap-4">
        <label class="grid gap-1 text-sm">
          {{ t('volume_hold_initial') }}
          <Input type="number" min="200" max="5000" step="100" class="w-28" data-hold-initial
            v-model="reglages.volume_repeat_initial_ms" />
        </label>
        <label class="grid gap-1 text-sm">
          {{ t('volume_hold_interval') }}
          <Input type="number" min="100" max="2000" step="50" class="w-28" data-hold-interval
            v-model="reglages.volume_repeat_interval_ms" />
        </label>
        <Button data-hold-change @click="enregistrerReglages">{{ t('change') }}</Button>
      </CardContent>
    </Card>
```
(The `Number(...)` in `enregistrerReglages` is what guarantees numbers on the wire even if the kit `Input`'s `v-model` yields strings — the third test pins that. If the kit `Input` does not forward `type`/`min`/`max` attributes, fall back to a plain `<input>` styled like the kit's — check `web/kit` first.)

- [ ] **Step 5: Run the web tests, then commit**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 10
git add web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): cartes de reglages demarrage et volume maintenu"
```

---

### Task 9: Sticky scrollspy table of contents on the config page

**Files:**
- Modify: `web/app/src/views/ConfigView.vue`
- Modify: `web/app/src/views/ConfigView.test.ts`
- Modify: `crates/ritornello-core/src/locales/en.toml`, `deploy/locales/core/fr.toml` (one key)

**Interfaces:**
- Consumes: the six cards (plugins, audio, language, startup, volume hold, logs) from Tasks 7–8.
- Produces: `<nav data-toc>` with one `data-toc-link` per section; each card wrapped in `<section :id>` with `scroll-mt-6`.

- [ ] **Step 1: i18n key**

`en.toml`: `toc_label = "sections"`. `fr.toml`: `toc_label = "sections"`.

- [ ] **Step 2: Write the failing tests**

jsdom has no `IntersectionObserver`: stub it in `monter()` (before mounting) and capture the callback:
```ts
type IOCallback = (entries: Array<{ target: Element; isIntersecting: boolean }>) => void
let ioCallback: IOCallback | null = null
class FauxIO {
  constructor(cb: IOCallback) { ioCallback = cb }
  observe() {}
  disconnect() {}
}
// dans monter(), avant mount :
vi.stubGlobal('IntersectionObserver', FauxIO)
```

```ts
describe('ConfigView — sommaire', () => {
  beforeEach(reinitialiser)

  it('liste une entrée par section, avec le libellé de sa carte', async () => {
    const { w } = await monter()
    const liens = w.findAll('[data-toc-link]')
    expect(liens.map((l) => l.text())).toEqual([
      'Plugins', 'Sortie audio', 'Langue', 'Démarrage', 'Volume maintenu', 'Dernières erreurs',
    ])
    // Masqué sur petit écran : la colonne fait max-w-3xl, pas la place en mobile.
    expect(w.find('[data-toc]').classes()).toContain('hidden')
  })

  it('un clic fait défiler en douceur vers la section et la marque active', async () => {
    const { w } = await monter()
    const scrollIntoView = vi.fn()
    const cible = w.find('#audio')
    expect(cible.exists()).toBe(true)
    cible.element.scrollIntoView = scrollIntoView
    await w.findAll('[data-toc-link]')[1]!.trigger('click')
    expect(scrollIntoView).toHaveBeenCalledWith({ behavior: 'smooth' })
    expect(w.findAll('[data-toc-link]')[1]!.attributes('aria-current')).toBe('true')
  })

  it('le défilement met à jour la section active (scrollspy)', async () => {
    const { w } = await monter()
    expect(ioCallback).not.toBeNull()
    ioCallback!([{ target: w.find('#language').element, isIntersecting: true }])
    ioCallback!([{ target: w.find('#plugins').element, isIntersecting: false }])
    await w.vm.$nextTick()
    const actifs = w.findAll('[data-toc-link][aria-current="true"]')
    expect(actifs).toHaveLength(1)
    expect(actifs[0]!.text()).toBe('Langue')
  })
})
```

- [ ] **Step 3: Run tests to verify they fail**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 15
```
Expected: FAIL (`data-toc` not found).

- [ ] **Step 4: Implement**

Script additions in `ConfigView.vue`:
```ts
/**
 * Le sommaire : une entrée par carte, dans l'ordre du gabarit. C'est une
 * donnée (comme REMOTE_ROWS pour la télécommande) : la vue la parcourt pour
 * le nav ET pour l'observation du défilement.
 */
const SECTIONS = [
  { id: 'plugins', key: 'plugins_title' },
  { id: 'audio', key: 'audio_output' },
  { id: 'language', key: 'language' },
  { id: 'startup', key: 'startup_title' },
  { id: 'volume-hold', key: 'volume_hold_title' },
  { id: 'logs', key: 'recent_errors' },
] as const

const active = ref<string>(SECTIONS[0].id)
// Visibilité par section, tenue à jour par l'observateur : la section active
// est la première visible dans l'ordre du sommaire (pas la dernière entrée
// reçue, qui dépend de l'ordre d'arrivée des callbacks).
const visibles = new Set<string>()
let observer: IntersectionObserver | null = null

onMounted(() => {
  observer = new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) visibles.add(e.target.id)
        else visibles.delete(e.target.id)
      }
      const premiere = SECTIONS.find((s) => visibles.has(s.id))
      if (premiere) active.value = premiere.id
    },
    // La bande d'observation est le haut de l'écran : la section « active »
    // est celle qu'on est en train de lire, pas celle qui pointe en bas.
    { rootMargin: '0px 0px -60% 0px' },
  )
  for (const s of SECTIONS) {
    const el = document.getElementById(s.id)
    if (el) observer.observe(el)
  }
})
onUnmounted(() => observer?.disconnect())

function aller(id: string) {
  active.value = id
  document.getElementById(id)?.scrollIntoView({ behavior: 'smooth' })
}
```
Add `onUnmounted` to the `vue` import.

Template: wrap the existing content and add the nav. Each card gets wrapped in `<section :id="..." class="scroll-mt-6">` (the `scroll-mt-6` keeps the scrolled-to card clear of the viewport edge):
```html
<template>
  <div class="flex gap-8">
    <div class="min-w-0 flex-1 space-y-4">
      <section id="plugins" class="scroll-mt-6"> <Card>…plugins…</Card> </section>
      <section id="audio" class="scroll-mt-6"> <Card>…sortie audio…</Card> </section>
      <section id="language" class="scroll-mt-6"> <Card>…langue…</Card> </section>
      <section id="startup" class="scroll-mt-6"> <Card>…démarrage…</Card> </section>
      <section id="volume-hold" class="scroll-mt-6"> <Card>…volume maintenu…</Card> </section>
      <section id="logs" class="scroll-mt-6"> <Card>…journaux…</Card> </section>
    </div>
    <nav data-toc :aria-label="t('toc_label')" class="sticky top-6 hidden w-40 shrink-0 self-start lg:block">
      <ul class="space-y-1 text-sm">
        <li v-for="s in SECTIONS" :key="s.id">
          <a
            :href="`#${s.id}`"
            data-toc-link
            :aria-current="active === s.id ? 'true' : undefined"
            :class="active === s.id ? 'font-medium text-foreground' : 'text-muted-foreground'"
            @click.prevent="aller(s.id)"
          >
            {{ t(s.key) }}
          </a>
        </li>
      </ul>
    </nav>
  </div>
</template>
```

- [ ] **Step 5: Run the web tests, then commit**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 10
git add web/app/src crates/ritornello-core/src/locales/en.toml deploy/locales/core/fr.toml
git commit -m "feat(web): sommaire lateral avec suivi du defilement sur la page config"
```

---

### Task 10: Hold-to-repeat on the web remote's volume buttons

**Files:**
- Modify: `web/app/src/views/HomeView.vue`
- Modify: `web/app/src/views/HomeView.test.ts`

**Interfaces:**
- Consumes: `GET /api/settings` (Task 4), `SettingsPayload` (Task 8).
- Produces: volume buttons carry `data-remote-hold="VolumeUp"` / `"VolumeDown"`.

- [ ] **Step 1: Write the failing tests** (in `HomeView.test.ts`)

```ts
describe('HomeView — volume maintenu', () => {
  /** Monte la vue avec des timings servis par /api/settings et des faux minuteurs. */
  async function monterAvecTimings(reglages = { volume_repeat_initial_ms: 1000, volume_repeat_interval_ms: 500, start_in_standby: false }) {
    vi.useFakeTimers()
    const posts: string[] = []
    const spy = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === 'POST') {
        posts.push(String(init.body))
        return new Response(null, { status: 204 })
      }
      if (url === '/api/settings') return new Response(JSON.stringify(reglages), { status: 200 })
      return new Response('{}', { status: 200 })
    })
    vi.stubGlobal('fetch', spy)
    const HomeView = (await import('./HomeView.vue')).default
    const w = mount(HomeView)
    // Laisse le GET /api/settings du montage se résoudre sous faux minuteurs.
    await vi.runOnlyPendingTimersAsync()
    return { w, posts }
  }

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  it('un appui simple envoie une seule commande', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerdown')
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerup')
    await vi.advanceTimersByTimeAsync(5000)
    expect(posts).toEqual([JSON.stringify({ cmd: 'VolumeUp' })])
  })

  it('un appui maintenu répète après le délai initial puis à l’intervalle', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeDown"]').trigger('pointerdown')
    expect(posts).toHaveLength(1) // le pas immédiat
    await vi.advanceTimersByTimeAsync(999)
    expect(posts).toHaveLength(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(posts).toHaveLength(2) // premier pas répété à 1000 ms
    await vi.advanceTimersByTimeAsync(500)
    expect(posts).toHaveLength(3)
    await vi.advanceTimersByTimeAsync(500)
    expect(posts).toHaveLength(4)
    await w.find('[data-remote-hold="VolumeDown"]').trigger('pointerup')
    await vi.advanceTimersByTimeAsync(5000)
    expect(posts).toHaveLength(4) // plus rien après le relâchement
  })

  it('les timings viennent de /api/settings', async () => {
    const { w, posts } = await monterAvecTimings({ volume_repeat_initial_ms: 200, volume_repeat_interval_ms: 100, start_in_standby: false })
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerdown')
    await vi.advanceTimersByTimeAsync(200)
    expect(posts).toHaveLength(2)
    await vi.advanceTimersByTimeAsync(100)
    expect(posts).toHaveLength(3)
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerup')
  })

  it('quitter le bouton pendant le maintien arrête la répétition', async () => {
    const { w, posts } = await monterAvecTimings()
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerdown')
    await w.find('[data-remote-hold="VolumeUp"]').trigger('pointerleave')
    await vi.advanceTimersByTimeAsync(5000)
    expect(posts).toHaveLength(1)
  })
})
```

Also update the existing `rend une rangée par groupe` expectations if needed (rows count unchanged; the volume buttons just gain attributes).

- [ ] **Step 2: Run tests to verify they fail**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 15
```
Expected: FAIL (`data-remote-hold` not found).

- [ ] **Step 3: Implement in `HomeView.vue`**

Script additions:
```ts
// Timings du volume maintenu, servis par le cœur (modifiables sur la page
// config). Les défauts couvrent le temps du GET et son éventuel échec.
const reglages = ref<SettingsPayload>({
  volume_repeat_initial_ms: 1000,
  volume_repeat_interval_ms: 500,
  start_in_standby: false,
})
onMounted(async () => {
  reglages.value = await api.get<SettingsPayload>('/api/settings').catch(() => reglages.value)
})

// Appui maintenu sur Volume +/- : un pas immédiat, puis après le délai
// initial un pas par intervalle jusqu'au relâchement. Miroir côté navigateur
// du cadencement que le cœur applique aux répétitions de la télécommande
// infrarouge — les timings sont les mêmes, servis par /api/settings.
let minuteurInitial: number | null = null
let minuteurIntervalle: number | null = null

function estVolume(c: RemoteCommand) {
  return c.cmd.cmd === 'VolumeUp' || c.cmd.cmd === 'VolumeDown'
}

function debutMaintien(cmd: Command) {
  finMaintien()
  send(cmd)
  minuteurInitial = window.setTimeout(() => {
    send(cmd)
    minuteurIntervalle = window.setInterval(() => send(cmd), reglages.value.volume_repeat_interval_ms)
  }, reglages.value.volume_repeat_initial_ms)
}

function finMaintien() {
  if (minuteurInitial !== null) { window.clearTimeout(minuteurInitial); minuteurInitial = null }
  if (minuteurIntervalle !== null) { window.clearInterval(minuteurIntervalle); minuteurIntervalle = null }
}

onUnmounted(finMaintien)
```
Imports to add: `onUnmounted`, `ref` from `vue`; `SettingsPayload` from `../types`; `RemoteCommand` type from `./remoteCommands`.

Template — in the `REMOTE_ROWS` loop, split volume buttons from the rest:
```html
        <div v-for="(rangee, i) in REMOTE_ROWS" :key="i" class="flex flex-wrap gap-2" data-remote-row>
          <template v-for="c in rangee" :key="c.key">
            <!-- Volume +/- : appui maintenu (pointeur) au lieu d'un clic. Pas
                 de @click : il partirait en double après le pointerup. Le
                 clavier garde un pas par touche via @keydown. touch-none
                 empêche le défilement tactile d'avaler le maintien,
                 @contextmenu.prevent le menu d'appui long mobile. -->
            <Button
              v-if="estVolume(c)"
              :data-remote-hold="c.cmd.cmd"
              variant="outline"
              class="touch-none select-none"
              @pointerdown="debutMaintien(c.cmd)"
              @pointerup="finMaintien"
              @pointercancel="finMaintien"
              @pointerleave="finMaintien"
              @contextmenu.prevent
              @keydown.enter.prevent="send(c.cmd)"
              @keydown.space.prevent="send(c.cmd)"
            >
              {{ t(c.key) }}
            </Button>
            <Button v-else variant="outline" @click="send(c.cmd)">{{ t(c.key) }}</Button>
          </template>
        </div>
```

- [ ] **Step 4: Run the web tests, then commit**

```powershell
npm run test -w app 2>&1 | Select-Object -Last 10
git add web/app/src
git commit -m "feat(web): appui maintenu sur les boutons volume de la telecommande"
```

---

### Task 11: Docs, full validation, e2e

**Files:**
- Modify: `docs/plugins.md` (input protocol section), `docs/interface.md` (page name + new cards)
- Check: any other `/status` or "status page" mention (`grep -ri "status" docs/ README.md`)

- [ ] **Step 1: Documentation**

- `docs/plugins.md`, input-plugin section: document the line format as `InputMessage` — a `Command` plus an optional `"held": true` for kernel autorepeat of volume-bound keys; absent = fresh press; the core paces held volume commands (initial delay then interval, set on the config page) and ignores `held` on other commands. Note explicitly that existing plugins writing bare `Command` lines keep working unchanged.
- `docs/interface.md`: the status page is now the config page (`/config`, `/status` redirects); describe the TOC, the startup card (on/standby at launch, on by default) and the volume-hold card (bounds 200–5000 / 100–2000 ms), and that the web remote's volume buttons repeat on hold with the same timings.
- Sweep `grep -rin "status" docs/ README.md` for stale references to the page (the `/api/status` endpoint keeps its name — do not rename it in the docs).

- [ ] **Step 2: Full builds and test suites**

```powershell
npm run build --workspaces
npm run test --workspaces 2>&1 | Select-Object -Last 10
npm run typecheck -w app
wsl.exe -- bash -lc "cd /mnt/c/projets/perso/ritornello/.claude/worktrees/config-page-et-reglages && touch crates/ritornello-core/build.rs && cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace --all-targets 2>&1 | tail -5"
```
Expected: everything green (the `touch` forces re-embedding the freshly built SPA).

- [ ] **Step 3: e2e**

```powershell
npm run e2e -w app 2>&1 | Select-Object -Last 15
```
Expected: all pass (the suite runs the real Rust binary; `/config` is served by the shell fallback, and the renamed navigation is covered by the updated `parcours.spec.ts`).

- [ ] **Step 4: Commit, then integrate**

```powershell
git add docs
git commit -m "docs: page config, reglages et protocole input avec held"
```
Then fast-forward `main` (no merge commit — repo policy):
```powershell
git -C C:\projets\perso\ritornello switch main
git -C C:\projets\perso\ritornello merge --ff-only worktree-config-page-et-reglages
```
(Only after the review of the whole branch — see the execution skill's checkpoints.)
