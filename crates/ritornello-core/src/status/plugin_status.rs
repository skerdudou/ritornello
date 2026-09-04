//! The plugins as seen from the status page: one line per (name, kind), the order of the plugins.toml file, the enabled/disabled switch, and what a disconnection or a re-announcement changes.

use super::*;

/// One line of the status page: a (name, kind) pair.
///
/// `stalled` distinguishes **three** states where two were not enough:
///
/// - `connected: true` — announced and wired;
/// - `connected: false` alone — process dead before announcing itself;
/// - `connected: false` + `stalled: true` — process **alive**, silent at the
///   rendezvous deadline.
///
/// A stalled plugin is not a dead plugin: it runs, it has said nothing, and it
/// may still speak — the registration socket stays open for it and the core
/// will hotplug it. That difference is what the operator must see.
///
/// The field is additive, with the idiom already used for `InputMessage.held`:
/// absent from the JSON when false, so no existing frame changes and an old
/// frame reads back without error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub admin: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stalled: bool,
    /// Launched just now, not yet announced, and **within the normal delay**.
    ///
    /// Exclusive with `stalled`, and both say the same thing about the plugin —
    /// it has not spoken. They differ only by the elapsed time, and that
    /// difference is everything: "stalled" accuses a faulty plugin, whereas a
    /// binary that takes two seconds to bind its sockets on an SD card is
    /// perfectly healthy. Showing "stalled" during a normal startup was
    /// therefore a wrongful accusation, reported in use.
    ///
    /// The switch to `stalled` is made by the core loop after `STARTUP_TIMEOUT`
    /// (see `main.rs`), and only if the line still says "starting" at that
    /// instant.
    ///
    /// Additive like the other two: absent from the JSON when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub starting: bool,
    /// Plugin switched off from the UI: no process, no wiring, and the manifest
    /// carries `enabled = false`. The line stays displayed — without it, it
    /// could no longer be switched back on.
    ///
    /// Additive like `stalled`: absent from the JSON when false, so no existing
    /// frame changes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Reachable plugin whose admin page does not answer the `Ping`: a long
    /// `set_data` holds its lock (most often a network share). Computed at
    /// `/api/status` time, never stored: it is a state that changes by the
    /// second. Additive like `stalled` and `disabled`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
    /// Fingerprint of the plugin's UI assets, as its announcement gave it.
    ///
    /// Relayed and never recomputed: the plugin is the only one holding those
    /// bytes at announcement time. The shell turns it into
    /// `/plugins/<name>/ui.js?v=<fingerprint>`, an URL that never needs
    /// revalidating.
    ///
    /// `None` = no admin page, or a plugin predating the field: the shell then
    /// builds the plain URL and the old revalidation applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_version: Option<String>,
}

impl PluginStatus {
    /// Line of an announced kind, reachable (`connected: true`) or not.
    ///
    /// `stalled` has no meaning here: the plugin has spoken. No derived
    /// `Default` on this struct for the same reason — a status without name or
    /// kind means nothing.
    pub fn kind(name: &str, kind: &str, connected: bool, admin: bool) -> Self {
        Self {
            name: name.to_string(),
            kind: kind.to_string(),
            connected,
            admin,
            stalled: false,
            starting: false,
            disabled: false,
            busy: false,
            ui_version: None,
        }
    }

    /// Line of a plugin that announced **no** kind: never launched, dead before
    /// the announcement, or alive and silent (`stalled`).
    ///
    /// The kind is reported as "unknown" rather than invented: the manifest no
    /// longer carries it, the binary is what announces it.
    pub fn unknown_kind(name: &str, stalled: bool) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled,
            starting: false,
            disabled: false,
            busy: false,
            ui_version: None,
        }
    }

    /// Line of a plugin that was just launched: it has not spoken, and that is
    /// normal.
    ///
    /// Distinct from `unknown_kind(name, true)`, which accuses. See the doc of
    /// the `starting` field.
    pub fn startup(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled: false,
            starting: true,
            disabled: false,
            busy: false,
            ui_version: None,
        }
    }

    /// Line of a switched-off plugin. Neither kind nor admin page: it announced
    /// nothing and will announce nothing until it is switched back on.
    pub fn disabled(name: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: "unknown".into(),
            connected: false,
            admin: false,
            stalled: false,
            starting: false,
            disabled: true,
            busy: false,
            ui_version: None,
        }
    }
}

/// Switch-on or switch-off order, from the HTTP layer to the core loop.
///
/// The acknowledgement is a `oneshot` and not a mere send: the page waits for
/// a response describing a state that is already true, otherwise it would
/// refresh on an intermediate state. `bool` and not `Result`: the only thing
/// the core can fail at is launching a binary, whose exact cause goes to the
/// log — which the UI already shows — while the screen receives a message from
/// the catalog.
pub struct PluginOrder {
    pub name: String,
    pub active: bool,
    pub ack: tokio::sync::oneshot::Sender<bool>,
}

/// What the HTTP layer must know about the plugins to toggle them.
///
/// A single `AppState` field rather than three, for the reason already retained
/// for `system`: every test constructor would otherwise grow by three lines.
pub struct PluginsControl {
    /// Path of `plugins.toml`: that is where the choice is written.
    pub manifest: std::path::PathBuf,
    /// Declared names, in file order. Authority on what may be toggled: an
    /// absent name is refused **before** any write.
    pub names: Vec<String>,
    pub tx: mpsc::Sender<PluginOrder>,
}

#[derive(Deserialize)]
pub(super) struct PluginEnabledReq {
    enabled: bool,
}

/// Toggles a plugin, **persistence first**.
///
/// The order of the three steps is the heart of the matter: a refused name
/// writes nothing, a failed write kills no process, and the core is only told
/// of a choice already on disk. A switched-off plugin whose switch-off had not
/// been written would come back at the next boot — a silent lie, worse than a
/// frank refusal.
pub(super) async fn plugin_enabled_put(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(req): Json<PluginEnabledReq>,
) -> Response {
    if !state.plugins.names.iter().any(|n| n == &name) {
        let msg = state.catalog.read().await.get("plugin_unknown").replace("{name}", &name);
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    if let Err(e) = crate::plugins::set_enabled(&state.plugins.manifest, &name, req.enabled) {
        tracing::warn!("persisting the enabled flag of {name}: {e:#}");
        let msg = state.catalog.read().await.get("plugin_persist_failed").replace("{name}", &name);
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg })))
            .into_response();
    }
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let order = PluginOrder { name: name.clone(), active: req.enabled, ack: ack_tx };
    if state.plugins.tx.send(order).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match ack_rx.await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        // The core refused (binary not found on switch-on) or did not answer.
        // The exact cause is in the log, which the UI already shows.
        _ => {
            let msg = state.catalog.read().await.get("plugin_action_failed").replace("{name}", &name);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": msg })))
                .into_response()
        }
    }
}

/// Marks the plugin `name` as disconnected in the status state: a plugin whose
/// process has exited is no longer reachable (supervision, live status page).
/// No-op if the name is unknown.
/// The `stalled` **and** `starting` flags are removed on the way, for the same
/// reason: both describe a *living* process — "stalled" means alive and
/// silent, "starting" means alive and not yet announced. A process whose exit
/// was just seen is no longer alive, and leaving either one would tell of a
/// state that does not exist.
///
/// **`admin` drops too**, and that is what removes the entry from the top
/// menu. The UI builds that menu with `plugins.filter(p => p.admin)` **without
/// looking at `connected`**: a line left at `admin: true` therefore kept
/// offering the page of a dead plugin, and the click returned an error instead
/// of nothing. It is the same symptom as for a switched-off plugin, settled on
/// its side because `disabled` sets the flag to false by construction. Useful
/// corollary: the `Ping` probe of `/api/status` filters only on `admin`, so it
/// stops querying a dead backend on every refresh at the same time.
///
/// `starting` in particular had a visible consequence: `main::should_downgrade`
/// consults only that flag, so a plugin that died **during** its ten seconds of
/// grace kept its "starting" line until the deadline, then got downgraded to
/// "stalled" — that is, announcing as alive but silent a process whose exit had
/// been reaped ten seconds earlier.
pub fn mark_plugin_disconnected(state: &mut StatusState, name: &str) {
    for p in &mut state.plugins {
        if p.name == name {
            p.connected = false;
            p.stalled = false;
            p.starting = false;
            p.admin = false;
        }
    }
}

/// Replaces **all** the lines of plugin `name` with `lines`.
///
/// Used at hotplug. The replacement is not a detail: a plugin restarted by
/// hand re-announces itself, and an insertion on top of the existing lines
/// would make it accumulate duplicates in the status page at every restart —
/// up to an unreadable page on a device that is never rebooted.
///
/// The new lines are placed where the old ones were, so that the displayed
/// order does not jump from one rewiring to the next.
///
/// An **empty** list does not make the plugin disappear: it leaves it visible
/// as unknown kind, not reachable. An announcement with `kinds: []` comes from
/// a badly compiled binary, and removing its lines without inserting any would
/// make it invisible right after it spoke — the exact opposite of what hotplug
/// exists to show. `stalled` stays false: it has just spoken, it is not silent.
///
/// `admin` is used **only** in that fallback case, and it is indispensable
/// there: `PluginStatus::unknown_kind` sets the flag to false by construction,
/// so a plugin announcing `kinds: []` **and** `admin: true` — a badly compiled
/// binary, but whose admin page is indeed reachable — saw its backend wired
/// with nothing in the UI leading to it. The non-empty lines already carry
/// their own flag, the caller having set it kind by kind.
pub fn replace_plugin_lines(
    state: &mut StatusState,
    name: &str,
    lines: Vec<PluginStatus>,
    admin: bool,
) {
    let place = state.plugins.iter().position(|p| p.name == name);
    state.plugins.retain(|p| p.name != name);
    let place = place.unwrap_or(state.plugins.len()).min(state.plugins.len());
    let lines = if lines.is_empty() {
        vec![PluginStatus { admin, ..PluginStatus::unknown_kind(name, false) }]
    } else {
        lines
    };
    for (i, line) in lines.into_iter().enumerate() {
        state.plugins.insert(place + i, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::settings_validation::SettingsError;
    use crate::status::tests_support::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    /// Rig with a real temporary `plugins.toml` and the core's ear kept: the
    /// two things the route touches.
    fn app_state_with_plugins(
    ) -> (AppState, tempfile::TempDir, tokio::sync::mpsc::Receiver<PluginOrder>) {
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
            plugins: Arc::new(PluginsControl {
                manifest: path,
                names: vec!["radio".into(), "cd".into()],
                tx,
            }),
            ..app_state()
        };
        (state, dir, rx)
    }

    #[tokio::test]
    async fn switching_off_persists_then_tells_the_core() {
        let (state, dir, mut rx) = app_state_with_plugins();
        let app = router(state.clone());
        // The core: it acknowledges receipt, like the main loop.
        let core = tokio::spawn(async move {
            let order = rx.recv().await.unwrap();
            assert_eq!(order.name, "cd");
            assert!(!order.active);
            let _ = order.ack.send(true);
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
        core.await.unwrap();
        let after = std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap();
        assert!(after.contains("enabled = false"), "{after}");
    }

    #[tokio::test]
    async fn an_explicit_refusal_from_the_core_returns_500_with_a_catalog_message() {
        // Switch-on without a binary at the `exec` path: the core answers
        // `false`, not a closed channel. The only branch of `ack_rx` that
        // remained uncovered.
        let (state, _dir, mut rx) = app_state_with_plugins();
        let app = router(state);
        let core = tokio::spawn(async move {
            let order = rx.recv().await.unwrap();
            let _ = order.ack.send(false);
        });

        let resp = app
            .oneshot(
                Request::put("/api/plugins/radio/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        core.await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // A catalog message, never a raw key.
        assert!(v["error"].as_str().unwrap().contains("radio"));
    }

    #[tokio::test]
    async fn an_undeclared_name_is_refused_without_writing_anything() {
        let (state, dir, _rx) = app_state_with_plugins();
        let before = std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap();
        let app = router(state);

        let resp = app
            .oneshot(
                Request::put("/api/plugins/never-seen/enabled")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // A message, never a catalog key.
        assert!(v["error"].as_str().unwrap().contains("never-seen"));
        assert_eq!(std::fs::read_to_string(dir.path().join("plugins.toml")).unwrap(), before);
    }

    #[tokio::test]
    async fn an_impossible_persistence_does_not_touch_the_runtime() {
        let (mut state, dir, mut rx) = app_state_with_plugins();
        // Manifest not found: the write will fail.
        state.plugins = Arc::new(PluginsControl {
            manifest: dir.path().join("absent.toml"),
            names: vec!["radio".into()],
            tx: state.plugins.tx.clone(),
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
        // Nothing was asked of the core: a killed plugin whose switch-off is
        // not persisted would come back at the next boot.
        assert!(rx.try_recv().is_err());
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // A catalog message, never a raw key: without this assertion, a typo
        // in `plugin_persist_failed` would let the suite stay green with a raw
        // key on screen.
        assert!(v["error"].as_str().unwrap().contains("radio"));
    }

    /// No refusal key can reach the screen as is.
    ///
    /// The `message()` tests resolve against an **ad hoc** catalog, which
    /// proves the interpolation but not that the key written in the code really
    /// exists: `Catalog::get` returns the key when it does not find it, so a
    /// typo would produce a toast displaying
    /// "settings_initial_delay_out_of_range" without any test complaining. The
    /// parity test between catalogs does not see it either: it compares the two
    /// files with each other, not with the code that calls them.
    ///
    /// This one therefore resolves each variant against the **English catalog
    /// actually embedded**, and refuses a message equal to its own key.
    #[test]
    fn every_refusal_resolves_against_the_embedded_catalog() {
        let catalog = Catalog::load("core", "en", std::path::Path::new("/nonexistent"), crate::i18n::EN);
        // A missing key is recognized by the message **being** the key: no
        // space, and the prefix it was given.
        let messages = [
            AudioOutputError::EmptyName.message(&catalog),
            SettingsError::InitialDelay { min: 200, max: 5000 }.message(&catalog),
            SettingsError::RepeatInterval { min: 100, max: 2000 }.message(&catalog),
            SettingsError::Overlay { min: 1000, max: 15000 }.message(&catalog),
            SettingsError::TensWindow { min: 1000, max: 15000 }.message(&catalog),
            SettingsError::SeekStep { min: 1, max: 120 }.message(&catalog),
        ];
        for m in &messages {
            assert!(
                m.contains(' '),
                "message reduced to a raw key, hence missing from the embedded catalog: {m:?}"
            );
        }
        // And the bounds do arrive interpolated, not as tokens.
        let bound = SettingsError::InitialDelay { min: 200, max: 5000 }.message(&catalog);
        assert!(bound.contains("200") && bound.contains("5000"), "bounds not interpolated: {bound:?}");
        assert!(!bound.contains("{min}") && !bound.contains("{max}"), "token left as is: {bound:?}");
    }

    #[test]
    fn busy_is_additive_absent_when_false() {
        let l = PluginStatus::kind("radio", "source", true, true);
        let json = serde_json::to_string(&l).unwrap();
        assert!(!json.contains("busy"), "{json}");
    }

    #[test]
    fn mark_plugin_disconnected_toggles_connected() {
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::kind("radio", "source", true, true),
                PluginStatus::kind("cd", "source", true, false),
            ],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "cd");
        assert!(!st.plugins.iter().find(|p| p.name == "cd").unwrap().connected);
        assert!(st.plugins.iter().find(|p| p.name == "radio").unwrap().connected);
        // Unknown name: no-op, does not panic.
        mark_plugin_disconnected(&mut st, "unknown");
    }

    #[test]
    fn mark_plugin_disconnected_toggles_all_the_lines_of_a_multi_kind_plugin() {
        // A plugin may announce several kinds (for example input and display):
        // the status page then carries one line per (name, kind) for that same
        // name. `admin` is a boolean flag carried by each kind line, never a
        // kind in itself. `mark_plugin_disconnected` already loops over all the
        // lines of the same name, but nothing proved it until now.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::kind("files", "input", true, true),
                PluginStatus::kind("files", "display", true, true),
                PluginStatus::kind("radio", "source", true, true),
            ],
            active_source: "files".into(),
        };
        mark_plugin_disconnected(&mut st, "files");
        assert!(
            st.plugins.iter().filter(|p| p.name == "files").all(|p| !p.connected),
            "both lines of files must toggle"
        );
        assert!(
            st.plugins.iter().find(|p| p.name == "radio").unwrap().connected,
            "the lines of another plugin must not be touched"
        );
    }

    #[test]
    fn mark_plugin_disconnected_clears_the_stalled_flag() {
        // A stalled plugin that dies later: `plugin_waits` sees it, and its
        // lines must stop announcing "alive but silent". Both flags together
        // would describe a state that does not exist.
        let mut st = StatusState {
            plugins: vec![PluginStatus::unknown_kind("files", true)],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "files");
        let line = &st.plugins[0];
        assert!(!line.connected);
        assert!(!line.stalled, "a process whose exit was seen is no longer stalled");
    }

    #[test]
    fn mark_plugin_disconnected_clears_the_starting_flag() {
        // A plugin that dies **during** its ten seconds of grace. Without this
        // clearing, its line stayed "starting" until the deadline, and since
        // `main::should_downgrade` consults only that flag, the sweep then
        // downgraded it to "stalled": alive but silent, for a process whose
        // exit had been reaped. Both flags describe a living process, and that
        // is why both drop here.
        let mut st = StatusState {
            plugins: vec![PluginStatus::startup("cd")],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "cd");
        let line = &st.plugins[0];
        assert!(!line.connected);
        assert!(!line.starting, "a process whose exit was seen is no longer starting");
        assert!(!line.stalled, "and it is not stalled either");
    }

    #[test]
    fn mark_plugin_disconnected_removes_the_admin_page_from_the_menu() {
        // The top menu of the UI is `plugins.filter(p => p.admin)`, with no
        // regard for `connected`: without this clearing, the entry of a dead
        // plugin stayed on offer and the click returned an error instead of
        // nothing. Exactly the complaint already handled for the "switched off"
        // case.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::kind("files", "input", true, true),
                PluginStatus::kind("files", "display", true, true),
                PluginStatus::kind("radio", "source", true, true),
            ],
            active_source: "radio".into(),
        };
        mark_plugin_disconnected(&mut st, "files");
        assert!(
            st.plugins.iter().filter(|p| p.name == "files").all(|p| !p.admin),
            "all the lines of the dead plugin must stop announcing a page"
        );
        assert!(
            st.plugins.iter().find(|p| p.name == "radio").unwrap().admin,
            "the page of another plugin must not be touched"
        );
    }

    #[test]
    fn a_disabled_line_promises_nothing() {
        let l = PluginStatus::disabled("cd");
        assert!(l.disabled);
        assert!(!l.connected, "no process: nothing is reachable");
        assert!(!l.stalled, "it is not silent, it does not exist");
        assert!(!l.admin, "no admin page to reach");
        assert_eq!(l.kind, "unknown");
    }

    #[test]
    fn disabled_is_omitted_when_false() {
        // `stalled` idiom: no existing frame changes shape.
        let json = serde_json::to_string(&PluginStatus::kind("radio", "source", true, false)).unwrap();
        assert!(!json.contains("disabled"), "{json}");
        let json = serde_json::to_string(&PluginStatus::disabled("cd")).unwrap();
        assert!(json.contains("\"disabled\":true"), "{json}");
    }

    #[test]
    fn a_re_announcement_replaces_the_plugin_lines_instead_of_adding_some() {
        // A plugin restarted by hand re-announces itself, and the core rewires
        // it. If it accumulated one more line every time, the status page of a
        // device that is never rebooted would end up unreadable.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::unknown_kind("files", true),
                PluginStatus::kind("radio", "source", true, true),
            ],
            active_source: "radio".into(),
        };
        // First announcement: the stalled one becomes two kind lines.
        replace_plugin_lines(
            &mut st,
            "files",
            vec![
                PluginStatus::kind("files", "source", true, true),
                PluginStatus::kind("files", "input", true, true),
            ],
            true,
        );
        // Re-announcement, this time without the `input` kind.
        replace_plugin_lines(
            &mut st,
            "files",
            vec![PluginStatus::kind("files", "source", true, true)],
            true,
        );

        assert_eq!(
            st.plugins.iter().filter(|p| p.name == "files").count(),
            1,
            "lines must not accumulate from one re-announcement to the next"
        );
        assert_eq!(st.plugins.len(), 2, "the other plugins stay intact");
        assert!(st.plugins.iter().any(|p| p.name == "radio"));
        // The plugin's place in the list does not jump from one rewiring to
        // the next: `files` was first, it stays first.
        assert_eq!(st.plugins[0].name, "files");
    }

    #[test]
    fn an_announcement_with_no_kind_leaves_the_plugin_visible() {
        // `kinds: []`: badly compiled plugin, or binary that is mistaken.
        // Removing its lines without inserting any made it **disappear** from
        // the page right after it spoke — a faulty plugin turned invisible, the
        // opposite of what this wiring exists to show.
        let mut st = StatusState {
            plugins: vec![
                PluginStatus::kind("files", "source", true, false),
                PluginStatus::kind("radio", "source", true, false),
            ],
            active_source: "radio".into(),
        };
        replace_plugin_lines(&mut st, "files", vec![], false);

        assert_eq!(st.plugins.len(), 2, "the plugin stays in the page");
        let line = st.plugins.iter().find(|p| p.name == "files").unwrap();
        assert_eq!(line.kind, "unknown");
        assert!(!line.connected);
        assert!(!line.stalled, "it has just spoken: it is not silent");
        assert_eq!(st.plugins[0].name, "files", "and it keeps its place");
    }

    #[test]
    fn an_announcement_with_no_kind_keeps_its_admin_flag() {
        // `kinds: []` **and** `admin: true`: the binary is badly compiled but
        // its admin page is reachable, so the backend is wired. The fallback
        // line comes from `unknown_kind`, whose `admin` is false by
        // construction: without the flag carried up to here, the UI displayed
        // no link to a page that exists — the exact opposite of what the rule
        // "the flag follows what was reached" was after.
        let mut st = StatusState { plugins: vec![], active_source: String::new() };
        replace_plugin_lines(&mut st, "files", vec![], true);

        let line = &st.plugins[0];
        assert_eq!(line.kind, "unknown");
        assert!(!line.connected, "no kind was reached");
        assert!(line.admin, "the admin page is reachable: the link must appear");
    }

    #[test]
    fn the_stalled_flag_is_absent_from_the_json_when_false() {
        // Additive field: the frame of a wired plugin does not change by one
        // byte, and an old frame reads back without error.
        let wired = PluginStatus::kind("radio", "source", true, true);
        assert_eq!(
            serde_json::to_string(&wired).unwrap(),
            r#"{"name":"radio","kind":"source","connected":true,"admin":true}"#
        );
        let stalled = PluginStatus::unknown_kind("files", true);
        assert_eq!(
            serde_json::to_string(&stalled).unwrap(),
            r#"{"name":"files","kind":"unknown","connected":false,"admin":false,"stalled":true}"#
        );
        let old: PluginStatus = serde_json::from_str(
            r#"{"name":"radio","kind":"source","connected":false,"admin":false}"#,
        )
        .unwrap();
        assert!(!old.stalled);
    }
}
