//! Plugin builder: one half is registered per kind, each
//! binding its socket immediately, then `run()` announces and serves.
//!
//! The "bind first, announce after" order is not a guideline but a
//! property of this type: the methods bind, only `run()` writes the announcement.
//! A plugin therefore cannot announce a kind whose socket is not ready.

use crate::server::{
    bind_admin, bind_display, bind_input, bind_metadata, bind_source, serve_admin, serve_display,
    serve_input, serve_metadata, serve_source, AdminPlugin, DisplayPlugin, InputPlugin,
    MetadataPlugin, SourcePlugin,
};
use anyhow::{Context, Result};
// `StreamExt` for the `.next()` of `run()`'s `FuturesUnordered`.
use futures::StreamExt;
use ritornello_i18n::Layer;
use ritornello_proto::{Announcement, PluginKind};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// A half ready to serve: its kind, for the announcement, and its loop.
struct Half {
    kind: PluginKind,
    /// Does this half want the cover bytes? Always `false` outside of
    /// a display.
    ///
    /// Kept **here**, in the register of halves, and not in a separate
    /// field of the `Runtime`: this is what makes `covers` a value *derived*
    /// from what was registered, exactly like `kinds` and `admin` —
    /// `run()` computes all three from this register, in one
    /// expression each, so the announcement cannot describe anything other
    /// than what is actually served. The value is read by `display()`, before
    /// the plugin is moved into its loop: after that, it can no longer be
    /// queried.
    covers: bool,
    serve: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}

pub struct Runtime {
    name: String,
    register: PathBuf,
    prefix: PathBuf,
    halves: Vec<Half>,
    /// The admin page's loop, if `.admin()` was called. Outside of
    /// `halves`: `admin` is not a `PluginKind`, it's a flag of the
    /// announcement.
    admin: Option<Pin<Box<dyn Future<Output = Result<()>> + Send>>>,
    /// Fingerprint of the admin page's UI assets, computed in `.admin()`
    /// while the plugin is still in hand — see `ui_fingerprint` below.
    ui_version: Option<String>,
    /// This plugin's own embedded translation layers, language → parsed
    /// layer, confided through `.texts()` and never read from anywhere
    /// else — in particular never from disk: that is the core's job (see
    /// `Announcement.catalog`'s own doc). Empty when `.texts()` was never
    /// called, which is exactly right for a plugin with no text of its
    /// own — the announcement still carries `Some({})`, not `None`, since
    /// this SDK always knows to write the field (see `announcement()`).
    texts: HashMap<String, Layer>,
    /// Version of the **plugin's** crate, handed in by the caller.
    ///
    /// Not read from `env!` here: that macro expands where it is written, so a
    /// version read in this file would be the SDK's. The two match today only
    /// because the whole workspace shares one number — a coincidence that
    /// would turn into the announcement lying the day they diverge.
    version: &'static str,
    /// The plugin crate's own `repository` key, handed in by the caller for
    /// exactly the reason `version` is: `env!` — here `option_env!` — expands
    /// where it is written, and written in this file it would report the
    /// SDK's.
    ///
    /// Relayed verbatim, a full URL and not an `owner/repo` pair: this crate
    /// depends only on `ritornello-proto`, so it has no path to the core's
    /// `parse_repo_url` and no business inventing a second one. The core
    /// interprets what the manifest says; the SDK only carries it.
    repository: Option<&'static str>,
}

/// Fingerprint of a plugin's UI assets.
///
/// `DefaultHasher` and no crypto dependency: this is a cache key, not a
/// signature. Its instability across compiler versions costs, at worst, one
/// extra fetch after a rebuild — and a rebuild changes the bytes anyway.
pub fn ui_fingerprint(plugin: &impl AdminPlugin) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in ["ui.js", "ui.css"] {
        // The path itself is hashed too: without it, a plugin serving the same
        // body under both names would collide with one serving neither.
        path.hash(&mut h);
        plugin.asset(path).map(|(_, body)| body).hash(&mut h);
    }
    format!("{:x}", h.finish())
}

impl Runtime {
    /// Builds a `Runtime` from the arguments passed by the core.
    ///
    /// Called through [`crate::declare_runtime!`] and not by hand: the macro
    /// is what expands the two `env!` at the plugin's own call site, which is
    /// the only place they mean the plugin.
    pub fn from_args(version: &'static str, repository: Option<&'static str>) -> Result<Self> {
        Ok(Self::new(
            crate::plugin_name(),
            crate::register_socket(),
            crate::socket_prefix(),
            version,
            repository,
        ))
    }

    /// Useful for tests, which don't go through `std::env::args`.
    pub fn new(
        name: String,
        register: PathBuf,
        prefix: PathBuf,
        version: &'static str,
        repository: Option<&'static str>,
    ) -> Self {
        Self {
            name,
            register,
            prefix,
            halves: Vec::new(),
            admin: None,
            ui_version: None,
            texts: HashMap::new(),
            version,
            repository,
        }
    }

    /// Confides this plugin's own translation layers, so `run()` can derive
    /// `Announcement.catalog` from them instead of the plugin writing the
    /// field itself — the same invariant as `covers` and `ui_version`:
    /// **derived, never asked, so the announcement cannot lie.**
    ///
    /// `sources` maps a language code to that language's **raw TOML pack
    /// source**, exactly the form a plugin already holds it in
    /// (`include_str!("locales/en.toml")`, as `ritornello-plugin-cd` does
    /// today for English alone — see `CD_EN`). Parsed here with
    /// [`Layer::parse`], so the refusal below lives where the parse does,
    /// at **build time**, before a single socket is bound: a plugin that
    /// has text to confide must be caught here if that text is broken,
    /// rather than discovered later on a screen.
    ///
    /// Refuses (`Err`) in two cases, both meaning the same thing — a
    /// plugin that calls this method is declaring it has real text, and a
    /// declaration with nothing sound behind it is refused outright:
    /// - any layer fails to parse (invalid TOML);
    /// - the `en` layer is missing, or present but empty.
    ///
    /// A plugin with genuinely no text of its own simply never calls this
    /// method: `run()` still announces `catalog: Some({})` for it (see
    /// `announcement()`), never `None` — this method exists only to be
    /// called by a plugin that *does* have something to confide.
    pub fn texts(
        mut self,
        sources: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> Result<Self> {
        let mut layers = HashMap::new();
        for (lang, source) in sources {
            let layer = Layer::parse(source).with_context(|| {
                format!("plugin {}: the {lang} translation pack is not valid TOML", self.name)
            })?;
            layers.insert(lang.to_string(), layer);
        }
        let english_is_sound = layers.get("en").is_some_and(|l| !l.is_empty());
        anyhow::ensure!(
            english_is_sound,
            "plugin {}: a plugin registering translation layers must supply a non-empty English one",
            self.name
        );
        self.texts = layers;
        Ok(self)
    }

    pub fn source(mut self, plugin: impl SourcePlugin) -> Result<Self> {
        let l = bind_source(&crate::socket_kind(&self.prefix, PluginKind::Source))?;
        self.halves.push(Half {
            kind: PluginKind::Source,
            covers: false,
            serve: Box::pin(serve_source(l, plugin)),
        });
        Ok(self)
    }

    pub fn display(mut self, plugin: impl DisplayPlugin) -> Result<Self> {
        let l = bind_display(&crate::socket_kind(&self.prefix, PluginKind::Display))?;
        // Read **before** the move into `serve_display`, the only order
        // possible: after that, the plugin belongs to the future serving it and
        // nobody can query it anymore. This is also what makes the
        // flag impossible to falsify — there is no parameter to
        // fill in, only a plugin method to read.
        let covers = plugin.wants_covers();
        self.halves.push(Half {
            kind: PluginKind::Display,
            covers,
            serve: Box::pin(serve_display(l, plugin)),
        });
        Ok(self)
    }

    pub fn input(mut self, plugin: impl InputPlugin) -> Result<Self> {
        let l = bind_input(&crate::socket_kind(&self.prefix, PluginKind::Input))?;
        self.halves.push(Half {
            kind: PluginKind::Input,
            covers: false,
            serve: Box::pin(serve_input(l, plugin)),
        });
        Ok(self)
    }

    pub fn metadata(mut self, plugin: impl MetadataPlugin) -> Result<Self> {
        let l = bind_metadata(&crate::socket_kind(&self.prefix, PluginKind::Metadata))?;
        self.halves.push(Half {
            kind: PluginKind::Metadata,
            covers: false,
            serve: Box::pin(serve_metadata(l, plugin)),
        });
        Ok(self)
    }

    pub fn admin(mut self, plugin: impl AdminPlugin) -> Result<Self> {
        let l = bind_admin(&crate::admin_socket(&self.prefix))?;
        // Computed here and not in `run`: at that point the plugin has already
        // been moved into the serving future and its assets are out of reach.
        self.ui_version = Some(ui_fingerprint(&plugin));
        self.admin = Some(Box::pin(serve_admin(l, plugin)));
        Ok(self)
    }

    /// The announcement this runtime will write, split out of `run` so it can
    /// be read without binding anything: a test that had to open sockets to
    /// check one field would be testing the wrong thing.
    fn announcement(&self) -> Announcement {
        Announcement {
            name: self.name.clone(),
            kinds: self.halves.iter().map(|m| m.kind).collect(),
            admin: self.admin.is_some(),
            // Derived, like the two above: the only source is the
            // register of halves, so no path can announce covers
            // for a display that doesn't want any, nor the reverse.
            covers: self.halves.iter().any(|m| m.covers),
            ui_version: self.ui_version.clone(),
            protocol: ritornello_proto::PROTOCOL_VERSION,
            version: Some(self.version.to_string()),
            // Derived like the rest of this line: the caller handed in what
            // its own manifest says, and nothing here can invent one.
            repository: self.repository.map(str::to_string),
            // Always `Some(_)`, never `None`: a plugin built against this SDK
            // always knows to write this field, whether or not `.texts()`
            // was ever called (see that method's own doc, and
            // `Announcement.catalog`'s — `None` is reserved for a binary
            // that predates the field entirely, which cannot be this one).
            catalog: Some(
                self.texts.iter().map(|(lang, layer)| (lang.clone(), layer.as_map().clone())).collect(),
            ),
        }
    }

    /// Announces, then serves all halves until one of them stops.
    ///
    /// Each half runs in its own task: a failure of the admin
    /// page must not cut the audio, and vice versa — this is
    /// exactly what the `radio`, `files` and `generic-input` plugins
    /// used to do by hand before this constructor.
    ///
    /// **Refuses before connecting** if the serialised line would be over
    /// `ritornello_proto::ANNOUNCEMENT_MAX_BYTES`: the core enforces that
    /// same bound on its own read (see that constant's own doc — both sides
    /// must agree), and a plugin that wrote past it anyway would have its
    /// line silently dropped there, with nothing but a log line on the
    /// core's side to explain a process that announced and then never
    /// registered. A third-party author embedding a dozen languages is
    /// exactly the case this whole effort exists to let happen, so failing
    /// silently in the direction that *looks like* success is the one shape
    /// this could not take — the refusal here names the actual size and the
    /// bound, on the side that can act on it.
    pub async fn run(self) -> Result<()> {
        let announcement = self.announcement();
        let line = serde_json::to_string(&announcement)?;
        anyhow::ensure!(
            line.len() <= ritornello_proto::ANNOUNCEMENT_MAX_BYTES,
            "announcement for {} is {} bytes, over the {}-byte bound the core enforces \
             (ritornello_proto::ANNOUNCEMENT_MAX_BYTES) — trim the embedded translation layers",
            self.name,
            line.len(),
            ritornello_proto::ANNOUNCEMENT_MAX_BYTES
        );
        let mut stream = UnixStream::connect(&self.register)
            .await
            .with_context(|| format!("connecting to {}", self.register.display()))?;
        stream.write_all(format!("{line}\n").as_bytes()).await?;
        stream.shutdown().await?;
        drop(stream);
        tracing::info!("announced as {} ({:?})", announcement.name, announcement.kinds);

        // Each half is tracked **independently to the end**. Above all,
        // no `select_all` nor `try_join!`: the first half to return
        // control — even cleanly — would then terminate the whole plugin,
        // and the other tasks would be abandoned without their failure
        // ever being observed. This is exactly what the old hand-rolled
        // `generic-input` setup forbade, with a comment that already
        // spelled out the ban on `try_join!` in plain terms.
        //
        // `FuturesUnordered` gives the best of both: each half is
        // logged **as soon as** it ends, named, without that
        // stopping it from waiting on the others.
        let mut tasks = Vec::new();
        for m in self.halves {
            let name = format!("{:?}", m.kind).to_lowercase();
            tasks.push((name, tokio::spawn(m.serve)));
        }
        if let Some(admin) = self.admin {
            tasks.push(("admin".to_string(), tokio::spawn(admin)));
        }

        let mut running: futures::stream::FuturesUnordered<_> = tasks
            .into_iter()
            .map(|(name, task)| async move { (name, task.await) })
            .collect();

        let mut failures = 0usize;
        while let Some((name, outcome)) = running.next().await {
            match outcome {
                Ok(Ok(())) => tracing::info!("{name} half ended"),
                Ok(Err(e)) => {
                    failures += 1;
                    tracing::error!("{name} half failed: {e:#}");
                }
                // A panic is captured in the `JoinHandle` instead of
                // unwinding the other half's stack.
                Err(e) => {
                    failures += 1;
                    tracing::error!("{name} half panicked: {e}");
                }
            }
        }
        if failures > 0 {
            anyhow::bail!("{failures} plugin half(s) failed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::{Announcement, PlayerState, PluginKind};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    struct PlaceholderDisplay {
        received: Arc<Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl crate::DisplayPlugin for PlaceholderDisplay {
        async fn show(&mut self, state: PlayerState) -> anyhow::Result<()> {
            self.received.lock().unwrap().push(state);
            Ok(())
        }
    }

    /// A display that **overrides** `wants_covers`, and nothing else. The only
    /// difference from `PlaceholderDisplay` is this method, so it is indeed
    /// what the announcement must reflect.
    struct DisplayThatWantsCovers;

    #[async_trait::async_trait]
    impl crate::DisplayPlugin for DisplayThatWantsCovers {
        async fn show(&mut self, _state: PlayerState) -> anyhow::Result<()> {
            Ok(())
        }
        fn wants_covers(&self) -> bool {
            true
        }
    }

    struct PlaceholderInput {
        rx: tokio::sync::mpsc::Receiver<ritornello_proto::InputMessage>,
    }

    #[async_trait::async_trait]
    impl crate::InputPlugin for PlaceholderInput {
        async fn next_command(&mut self) -> anyhow::Result<ritornello_proto::InputMessage> {
            self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("channel closed"))
        }
    }

    /// Reads the single announcement deposited on a register socket.
    async fn read_announcement(listener: &UnixListener) -> Announcement {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let line = lines.next_line().await.unwrap().expect("an announcement");
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn the_announcement_describes_exactly_the_registered_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefix = dir.path().join("mpd");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new("mpd".into(), register.clone(), prefix.clone(), "0.0.0-test", None)
            .display(PlaceholderDisplay { received })
            .unwrap()
            .input(PlaceholderInput { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = read_announcement(&listener).await;
        assert_eq!(a.name, "mpd");
        assert_eq!(a.kinds, vec![PluginKind::Display, PluginKind::Input]);
        assert!(!a.admin, "no .admin() called");
        assert!(!a.covers, "no display overrode wants_covers");
    }

    /// The invariant the whole registration protocol rests on:
    /// the announcement is **derived** from what was registered, so it cannot
    /// lie. Exercised in both directions on the single flag this
    /// project adds, with two displays that differ *only* by
    /// `wants_covers`.
    ///
    /// The negative direction is the one that protects the console: a twenty-
    /// column display overrode nothing, and the core must therefore never push
    /// it megabytes.
    #[tokio::test]
    async fn the_covers_flag_is_derived_from_the_registered_display() {
        for (wants, plugin) in [(false, 0u8), (true, 1u8)] {
            let dir = tempfile::tempdir().unwrap();
            let register = dir.path().join("register.sock");
            let listener = UnixListener::bind(&register).unwrap();
            let prefix = dir.path().join("display");

            let rt = Runtime::new("display".into(), register.clone(), prefix.clone(), "0.0.0-test", None);
            let rt = if plugin == 0 {
                // Does not override `wants_covers`: the default body decides.
                rt.display(PlaceholderDisplay { received: Arc::new(Mutex::new(Vec::new())) })
                    .unwrap()
            } else {
                rt.display(DisplayThatWantsCovers).unwrap()
            };
            tokio::spawn(async move { rt.run().await.unwrap() });

            let a = read_announcement(&listener).await;
            assert_eq!(a.kinds, vec![PluginKind::Display]);
            assert_eq!(
                a.covers, wants,
                "the announcement must describe exactly what the registered display wants"
            );
        }
    }

    /// A kind without a display cannot announce covers: the flag
    /// is computed from the register of halves, where only a display can
    /// set `covers: true`.
    #[tokio::test]
    async fn a_plugin_without_a_display_never_announces_covers() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefix = dir.path().join("input");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let rt = Runtime::new("input".into(), register.clone(), prefix.clone(), "0.0.0-test", None)
            .input(PlaceholderInput { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = read_announcement(&listener).await;
        assert_eq!(a.kinds, vec![PluginKind::Input]);
        assert!(!a.covers);
    }

    #[tokio::test]
    async fn the_sockets_are_bound_before_the_announcement_is_readable() {
        // This is the central invariant of this project: when the core reads
        // the announcement, it can connect without retrying.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefix = dir.path().join("mpd");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let received = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new("mpd".into(), register.clone(), prefix.clone(), "0.0.0-test", None)
            .display(PlaceholderDisplay { received })
            .unwrap()
            .input(PlaceholderInput { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = read_announcement(&listener).await;
        // A BARE connect, with no retry loop: it must succeed on the first try.
        for kind in a.kinds {
            let path = crate::socket_kind(&prefix, kind);
            UnixStream::connect(&path)
                .await
                .unwrap_or_else(|e| panic!("{} refused the connection: {e}", path.display()));
        }
    }

    #[tokio::test]
    async fn two_kinds_are_served_by_the_same_process() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefix = dir.path().join("mpd");

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let received = Arc::new(Mutex::new(Vec::new()));
        let received_test = received.clone();
        let rt = Runtime::new("mpd".into(), register.clone(), prefix.clone(), "0.0.0-test", None)
            .display(PlaceholderDisplay { received })
            .unwrap()
            .input(PlaceholderInput { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });
        let _ = read_announcement(&listener).await;

        // Display side: the core pushes a state.
        let display = UnixStream::connect(crate::socket_kind(&prefix, PluginKind::Display))
            .await
            .unwrap();
        let (_r, mut w) = display.into_split();
        let frame = ritornello_proto::DisplayFrame::State(PlayerState::default());
        w.write_all(format!("{}\n", serde_json::to_string(&frame).unwrap()).as_bytes())
            .await
            .unwrap();

        // Input side: the plugin pushes a command.
        let input = UnixStream::connect(crate::socket_kind(&prefix, PluginKind::Input))
            .await
            .unwrap();
        tx.send(ritornello_proto::Command::Next.into()).await.unwrap();
        let mut lines = BufReader::new(input).lines();
        let line = lines.next_line().await.unwrap().expect("a command");
        assert!(line.contains("Next"), "unexpected command: {line}");

        for _ in 0..100 {
            if received_test.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the state did not reach the display even though the input was working");
    }

    #[test]
    fn the_runtime_announces_the_protocol_the_version_and_the_repository_it_was_given() {
        // Written from what the Runtime was constructed with, not from a
        // constant re-read here: the point of the parameters is that both come
        // from the plugin's crate, so a test that recomputed either locally
        // would prove nothing. Hence values that are deliberately **not** this
        // workspace's — `9.9.9` is no version this repository ever had, and
        // `someone/their-plugin` is no repository of ours.
        let r = Runtime::new(
            "radio".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/radio"),
            "9.9.9",
            Some("https://github.com/someone/their-plugin"),
        );
        let a = r.announcement();
        assert_eq!(a.protocol, ritornello_proto::PROTOCOL_VERSION);
        assert_eq!(a.version.as_deref(), Some("9.9.9"), "the version must be the plugin's, verbatim");
        assert_eq!(
            a.repository.as_deref(),
            Some("https://github.com/someone/their-plugin"),
            "the repository must be the plugin's manifest URL, verbatim and unparsed"
        );
    }

    /// A crate whose manifest carries no `repository` key — what a minimal
    /// third-party plugin looks like, and why `declare_runtime!` expands
    /// `option_env!` rather than `env!`.
    ///
    /// Its own test rather than a second assertion above: `None` and
    /// `Some(url)` are the two answers the core branches on, and a fixture
    /// that only ever saw one of them could not tell a relayed field from a
    /// hard-coded one.
    #[test]
    fn a_plugin_whose_manifest_names_no_repository_announces_none() {
        let r = Runtime::new(
            "minimal".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/minimal"),
            "1.0.0",
            None,
        );
        assert_eq!(r.announcement().repository, None);
    }

    #[test]
    fn the_fingerprint_follows_the_ui_bytes() {
        // Derived from what the plugin actually exposes, never declared: the
        // announcement cannot lie, exactly as for `covers`.
        struct Ui(&'static str);
        #[async_trait::async_trait]
        impl AdminPlugin for Ui {
            fn asset(&self, path: &str) -> Option<(String, String)> {
                match path {
                    "ui.js" => Some(("text/javascript".into(), self.0.to_string())),
                    "ui.css" => Some(("text/css".into(), ".a{}".into())),
                    _ => None,
                }
            }
            fn catalog(&self, _lang: Option<&str>) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn get_data(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn set_data(&mut self, _: serde_json::Value) -> Result<(), String> {
                Ok(())
            }
        }
        assert_ne!(ui_fingerprint(&Ui("one")), ui_fingerprint(&Ui("two")));
        assert_eq!(ui_fingerprint(&Ui("one")), ui_fingerprint(&Ui("one")));
    }

    /// The consequence spelled out in `Announcement.catalog`'s own doc: a
    /// plugin that never calls `.texts()` — `console`, `ouifm-metas` and
    /// `radiofrance-metas` today — still announces `Some({})`, never
    /// `None`. `None` is reserved for a binary that predates the field
    /// entirely, and a plugin built against this SDK never is one.
    #[test]
    fn a_plugin_that_never_calls_texts_announces_an_empty_catalog_not_none() {
        let r = Runtime::new(
            "console".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/console"),
            "0.2.0-test",
            None,
        );
        assert_eq!(r.announcement().catalog, Some(HashMap::new()));
    }

    /// The positive path: a plugin confiding two languages sees both, and
    /// exactly the keys and values it handed in — nothing invented, nothing
    /// dropped.
    #[test]
    fn texts_derives_the_catalog_from_the_confided_layers() {
        let r = Runtime::new(
            "cd".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/cd"),
            "0.2.0-test",
            None,
        )
        .texts([("en", "play = \"Play\"\n"), ("fr", "play = \"Lecture\"\n")])
        .unwrap();
        let catalog = r.announcement().catalog.expect("a plugin that confided text must announce it");
        assert_eq!(catalog.get("en").and_then(|l| l.get("play")).map(String::as_str), Some("Play"));
        assert_eq!(catalog.get("fr").and_then(|l| l.get("play")).map(String::as_str), Some("Lecture"));
    }

    /// **[MUTATION]** Barrier 6 of the spec: a plugin declaring text whose
    /// English pack is not even valid TOML must be refused at build time,
    /// before a socket is bound — not left to fail silently on screen.
    #[test]
    fn texts_refuses_an_english_layer_that_fails_to_parse() {
        let r = Runtime::new(
            "broken".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/broken"),
            "0.2.0-test",
            None,
        )
        .texts([("en", "this is not toml =")]);
        assert!(r.is_err(), "invalid TOML in the English pack must be refused, not swallowed");
    }

    /// **[MUTATION]** The same barrier again, isolated from its neighbour:
    /// a broken **non-English** layer, next to a perfectly sound English
    /// one. Mutation testing found this case matters on its own — a parse
    /// failure silently swallowed as an empty layer (`unwrap_or_default`)
    /// still passed `texts_refuses_an_english_layer_that_fails_to_parse`
    /// above, because the empty-English guard backstops it by accident when
    /// the *only* confided language is the broken one. With a sound English
    /// pack present, that backstop cannot fire, so only the parse guard
    /// itself can catch a broken `fr` here.
    #[test]
    fn texts_refuses_when_a_non_english_layer_fails_to_parse() {
        let r = Runtime::new(
            "broken".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/broken"),
            "0.2.0-test",
            None,
        )
        .texts([("en", "play = \"Play\"\n"), ("fr", "this is not toml =")]);
        assert!(r.is_err(), "invalid TOML in a non-English pack must be refused too, not swallowed");
    }

    /// **[MUTATION]** The other branch of the same barrier: an English pack
    /// that parses cleanly but defines **no key at all** is just as unsound
    /// as one that fails to parse — a plugin claiming to have text must
    /// actually have some.
    #[test]
    fn texts_refuses_an_empty_english_layer() {
        let r = Runtime::new(
            "broken".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/broken"),
            "0.2.0-test",
            None,
        )
        .texts([("en", "")]);
        assert!(r.is_err(), "an empty English pack must be refused, not accepted as sound");
    }

    /// **[MUTATION]** The third branch: a plugin confiding languages but no
    /// `en` entry at all — the key simply absent, not merely empty — must
    /// be refused the same way. Without this branch tested on its own, a
    /// guard that only checked "if present, non-empty" would pass every
    /// other test here while silently accepting a plugin with, say, only a
    /// French pack.
    #[test]
    fn texts_refuses_a_plugin_with_no_english_layer_at_all() {
        let r = Runtime::new(
            "broken".into(),
            std::path::PathBuf::from("/tmp/register.sock"),
            std::path::PathBuf::from("/tmp/broken"),
            "0.2.0-test",
            None,
        )
        .texts([("fr", "play = \"Lecture\"\n")]);
        assert!(r.is_err(), "a catalog with no English layer at all must be refused");
    }

    /// **[MUTATION]** The wire-size barrier: `run()` must refuse an
    /// announcement over `ANNOUNCEMENT_MAX_BYTES` **before ever connecting**,
    /// not let the core's own read cap silently drop the line. Proven from
    /// the event, not by calling a helper in isolation: a real listener is
    /// bound, `run()` is awaited to completion, and the test asserts both
    /// that it errs *and* that the listener never even saw a connection
    /// attempt — the failure this barrier exists to replace was exactly a
    /// process that looked like it succeeded (`tracing::info!("announced as
    /// …")` would have fired) while never actually registering. `run()`
    /// itself is awaited under a `tokio::time::timeout`: a regression of
    /// the guard sends `run()` into a real `write_all` of an oversized line
    /// on a socket nothing here reads from, which blocks rather than
    /// errors — so without the bound, this test would hang instead of
    /// failing on exactly the regression it exists to catch.
    #[tokio::test]
    async fn run_refuses_an_announcement_over_the_wire_bound_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefix = dir.path().join("heavy");

        // One key whose value alone already clears the bound: `.texts()`'s
        // own barrier only rejects an *unsound* English layer, and this one
        // is perfectly sound TOML — the size is the only thing wrong with
        // it. Leaked rather than borrowed: `.texts()` takes `&'static str`,
        // the same shape a real plugin's `include_str!` would hand it, and a
        // leak in a single test is a fair price for exercising that shape.
        let huge: &'static str = Box::leak(
            format!("play = \"{}\"\n", "x".repeat(ritornello_proto::ANNOUNCEMENT_MAX_BYTES + 1))
                .into_boxed_str(),
        );
        let rt = Runtime::new("heavy".into(), register.clone(), prefix, "0.0.0-test", None)
            .texts([("en", huge)])
            .unwrap();

        // Bounded, not a bare `.await`: without this, a regression of the
        // guard itself would not fail this test — it would **hang** it,
        // since `run()` would then reach a real `write_all` of 256 KiB+ on
        // a socket nobody here reads from (see this test's own doc, and the
        // mutation evidence in the task report: disabling the guard once
        // produced exactly this hang, discovered the hard way). A hung test
        // is not a failing test, and costs far more to diagnose in CI than
        // a red assertion with a clear message.
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), rt.run())
            .await
            .expect("run() must refuse within 2s rather than hang on an oversized announcement");
        let err = outcome.expect_err("an oversized announcement must be refused");
        let message = format!("{err:#}");
        assert!(message.contains("bytes"), "the refusal must name the actual size: {message}");
        assert!(
            message.contains(&ritornello_proto::ANNOUNCEMENT_MAX_BYTES.to_string()),
            "the refusal must name the bound itself: {message}"
        );

        // Nothing was ever attempted: a bare `accept` must find no one
        // waiting, proving `run()` bailed before `UnixStream::connect`.
        let accept = tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(
            accept.is_err(),
            "the oversized announcement must be refused before any connection is attempted"
        );
    }
}
