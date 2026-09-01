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
use ritornello_proto::{Announcement, PluginKind};
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
}

impl Runtime {
    /// Builds a `Runtime` from the arguments passed by the core.
    pub fn from_args() -> Result<Self> {
        Ok(Self::new(
            crate::plugin_name(),
            crate::register_socket(),
            crate::socket_prefix(),
        ))
    }

    /// Useful for tests, which don't go through `std::env::args`.
    pub fn new(name: String, register: PathBuf, prefix: PathBuf) -> Self {
        Self { name, register, prefix, halves: Vec::new(), admin: None }
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
        self.admin = Some(Box::pin(serve_admin(l, plugin)));
        Ok(self)
    }

    /// Announces, then serves all halves until one of them stops.
    ///
    /// Each half runs in its own task: a failure of the admin
    /// page must not cut the audio, and vice versa — this is
    /// exactly what the `radio`, `files` and `generic-input` plugins
    /// used to do by hand before this constructor.
    pub async fn run(self) -> Result<()> {
        let announcement = Announcement {
            name: self.name.clone(),
            kinds: self.halves.iter().map(|m| m.kind).collect(),
            admin: self.admin.is_some(),
            // Derived, like the two above: the only source is the
            // register of halves, so no path can announce covers
            // for a display that doesn't want any, nor the reverse.
            covers: self.halves.iter().any(|m| m.covers),
        };
        let mut stream = UnixStream::connect(&self.register)
            .await
            .with_context(|| format!("connecting to {}", self.register.display()))?;
        stream.write_all(format!("{}\n", serde_json::to_string(&announcement)?).as_bytes()).await?;
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
        let rt = Runtime::new("mpd".into(), register.clone(), prefix.clone())
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

            let rt = Runtime::new("display".into(), register.clone(), prefix.clone());
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
        let rt = Runtime::new("input".into(), register.clone(), prefix.clone())
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
        let rt = Runtime::new("mpd".into(), register.clone(), prefix.clone())
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
        let rt = Runtime::new("mpd".into(), register.clone(), prefix.clone())
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
}
