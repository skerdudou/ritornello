//! Constructeur d'un greffon : on enregistre une moitié par genre, chacune
//! liant son socket immédiatement, puis `run()` annonce et sert.
//!
//! L'ordre « lier d'abord, annoncer ensuite » n'est pas une consigne mais une
//! propriété de ce type : les méthodes lient, seul `run()` écrit l'annonce. Un
//! greffon ne peut donc pas annoncer un genre dont le socket n'est pas prêt.

use crate::server::{
    bind_admin, bind_display, bind_input, bind_metadata, bind_source, serve_admin, serve_display,
    serve_input, serve_metadata, serve_source, AdminPlugin, DisplayPlugin, InputPlugin,
    MetadataPlugin, SourcePlugin,
};
use anyhow::{Context, Result};
// `StreamExt` pour le `.next()` du `FuturesUnordered` de `run()`.
use futures::StreamExt;
use ritornello_proto::{Announcement, PluginKind};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

/// Une moitié prête à servir : son genre, pour l'annonce, et sa boucle.
struct Moitie {
    kind: PluginKind,
    servir: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}

pub struct Runtime {
    name: String,
    register: PathBuf,
    prefix: PathBuf,
    moities: Vec<Moitie>,
    /// La boucle de la page d'admin, si `.admin()` a été appelé. Hors de
    /// `moities` : `admin` n'est pas un `PluginKind`, c'est un drapeau de
    /// l'annonce.
    admin: Option<Pin<Box<dyn Future<Output = Result<()>> + Send>>>,
}

impl Runtime {
    /// Monte un `Runtime` depuis les arguments passés par le cœur.
    pub fn from_args() -> Result<Self> {
        Ok(Self::new(
            crate::plugin_name(),
            crate::register_socket(),
            crate::socket_prefix(),
        ))
    }

    /// Utile aux tests, qui ne passent pas par `std::env::args`.
    pub fn new(name: String, register: PathBuf, prefix: PathBuf) -> Self {
        Self { name, register, prefix, moities: Vec::new(), admin: None }
    }

    pub fn source(mut self, plugin: impl SourcePlugin) -> Result<Self> {
        let l = bind_source(&crate::genre_socket(&self.prefix, PluginKind::Source))?;
        self.moities.push(Moitie {
            kind: PluginKind::Source,
            servir: Box::pin(serve_source(l, plugin)),
        });
        Ok(self)
    }

    pub fn display(mut self, plugin: impl DisplayPlugin) -> Result<Self> {
        let l = bind_display(&crate::genre_socket(&self.prefix, PluginKind::Display))?;
        self.moities.push(Moitie {
            kind: PluginKind::Display,
            servir: Box::pin(serve_display(l, plugin)),
        });
        Ok(self)
    }

    pub fn input(mut self, plugin: impl InputPlugin) -> Result<Self> {
        let l = bind_input(&crate::genre_socket(&self.prefix, PluginKind::Input))?;
        self.moities.push(Moitie {
            kind: PluginKind::Input,
            servir: Box::pin(serve_input(l, plugin)),
        });
        Ok(self)
    }

    pub fn metadata(mut self, plugin: impl MetadataPlugin) -> Result<Self> {
        let l = bind_metadata(&crate::genre_socket(&self.prefix, PluginKind::Metadata))?;
        self.moities.push(Moitie {
            kind: PluginKind::Metadata,
            servir: Box::pin(serve_metadata(l, plugin)),
        });
        Ok(self)
    }

    pub fn admin(mut self, plugin: impl AdminPlugin) -> Result<Self> {
        let l = bind_admin(&crate::admin_socket(&self.prefix))?;
        self.admin = Some(Box::pin(serve_admin(l, plugin)));
        Ok(self)
    }

    /// Annonce, puis sert toutes les moitiés jusqu'à ce que l'une s'arrête.
    ///
    /// Chaque moitié tourne dans sa propre tâche : la panne de la page
    /// d'admin ne doit pas couper l'audio, et réciproquement — c'est
    /// exactement ce que les greffons `radio`, `files` et `generic-input`
    /// faisaient à la main avant ce constructeur.
    pub async fn run(self) -> Result<()> {
        let annonce = Announcement {
            name: self.name.clone(),
            kinds: self.moities.iter().map(|m| m.kind).collect(),
            admin: self.admin.is_some(),
        };
        let mut flux = UnixStream::connect(&self.register)
            .await
            .with_context(|| format!("connecting to {}", self.register.display()))?;
        flux.write_all(format!("{}\n", serde_json::to_string(&annonce)?).as_bytes()).await?;
        flux.shutdown().await?;
        drop(flux);
        tracing::info!("announced as {} ({:?})", annonce.name, annonce.kinds);

        // Chaque moitié est suivie **indépendamment jusqu'au bout**. Surtout
        // pas de `select_all` ni de `try_join!` : la première moitié qui rend
        // la main — même proprement — terminerait alors tout le greffon, et
        // les autres tâches seraient abandonnées sans que leur échec soit
        // jamais observé. C'est exactement ce que l'ancien montage à la main
        // de `generic-input` interdisait, avec un commentaire qui proscrivait
        // déjà `try_join!` en toutes lettres.
        //
        // `FuturesUnordered` donne le meilleur des deux : chaque moitié est
        // journalisée **dès** qu'elle se termine, en étant nommée, sans que
        // cela cesse d'attendre les autres.
        let mut taches = Vec::new();
        for m in self.moities {
            let nom = format!("{:?}", m.kind).to_lowercase();
            taches.push((nom, tokio::spawn(m.servir)));
        }
        if let Some(admin) = self.admin {
            taches.push(("admin".to_string(), tokio::spawn(admin)));
        }

        let mut en_cours: futures::stream::FuturesUnordered<_> = taches
            .into_iter()
            .map(|(nom, tache)| async move { (nom, tache.await) })
            .collect();

        let mut echecs = 0usize;
        while let Some((nom, resultat)) = en_cours.next().await {
            match resultat {
                Ok(Ok(())) => tracing::info!("{nom} half ended"),
                Ok(Err(e)) => {
                    echecs += 1;
                    tracing::error!("{nom} half failed: {e:#}");
                }
                // Une panique est capturée dans le `JoinHandle` au lieu de
                // dérouler la pile de l'autre moitié.
                Err(e) => {
                    echecs += 1;
                    tracing::error!("{nom} half panicked: {e}");
                }
            }
        }
        if echecs > 0 {
            anyhow::bail!("{echecs} plugin half(s) failed");
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

    struct AfficheurBouchon {
        recus: Arc<Mutex<Vec<PlayerState>>>,
    }

    #[async_trait::async_trait]
    impl crate::DisplayPlugin for AfficheurBouchon {
        async fn show(&mut self, state: PlayerState) -> anyhow::Result<()> {
            self.recus.lock().unwrap().push(state);
            Ok(())
        }
    }

    struct EntreeBouchon {
        rx: tokio::sync::mpsc::Receiver<ritornello_proto::InputMessage>,
    }

    #[async_trait::async_trait]
    impl crate::InputPlugin for EntreeBouchon {
        async fn next_command(&mut self) -> anyhow::Result<ritornello_proto::InputMessage> {
            self.rx.recv().await.ok_or_else(|| anyhow::anyhow!("canal ferme"))
        }
    }

    /// Lit l'unique annonce déposée sur un socket d'enregistrement.
    async fn lire_annonce(listener: &UnixListener) -> Announcement {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lignes = BufReader::new(stream).lines();
        let ligne = lignes.next_line().await.unwrap().expect("une annonce");
        serde_json::from_str(&ligne).unwrap()
    }

    #[tokio::test]
    async fn lannonce_decrit_exactement_les_genres_enregistres() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("mpd");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let recus = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new("mpd".into(), register.clone(), prefixe.clone())
            .display(AfficheurBouchon { recus })
            .unwrap()
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = lire_annonce(&listener).await;
        assert_eq!(a.name, "mpd");
        assert_eq!(a.kinds, vec![PluginKind::Display, PluginKind::Input]);
        assert!(!a.admin, "aucun .admin() appele");
    }

    #[tokio::test]
    async fn les_sockets_sont_lies_avant_que_lannonce_soit_lisible() {
        // C'est l'invariant central du chantier : quand le coeur lit
        // l'annonce, il peut se connecter sans retenter.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("mpd");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let recus = Arc::new(Mutex::new(Vec::new()));
        let rt = Runtime::new("mpd".into(), register.clone(), prefixe.clone())
            .display(AfficheurBouchon { recus })
            .unwrap()
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = lire_annonce(&listener).await;
        // Un connect NU, sans boucle de reprise : il doit aboutir du premier coup.
        for genre in a.kinds {
            let chemin = crate::genre_socket(&prefixe, genre);
            UnixStream::connect(&chemin)
                .await
                .unwrap_or_else(|e| panic!("{} refuse la connexion: {e}", chemin.display()));
        }
    }

    #[tokio::test]
    async fn deux_genres_sont_servis_par_le_meme_processus() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("mpd");

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let recus = Arc::new(Mutex::new(Vec::new()));
        let recus_test = recus.clone();
        let rt = Runtime::new("mpd".into(), register.clone(), prefixe.clone())
            .display(AfficheurBouchon { recus })
            .unwrap()
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });
        let _ = lire_annonce(&listener).await;

        // Cote afficheur : le coeur pousse un etat.
        let display = UnixStream::connect(crate::genre_socket(&prefixe, PluginKind::Display))
            .await
            .unwrap();
        let (_r, mut w) = display.into_split();
        w.write_all(format!("{}\n", serde_json::to_string(&PlayerState::default()).unwrap()).as_bytes())
            .await
            .unwrap();

        // Cote entree : le greffon pousse une commande.
        let input = UnixStream::connect(crate::genre_socket(&prefixe, PluginKind::Input))
            .await
            .unwrap();
        tx.send(ritornello_proto::Command::Next.into()).await.unwrap();
        let mut lignes = BufReader::new(input).lines();
        let ligne = lignes.next_line().await.unwrap().expect("une commande");
        assert!(ligne.contains("Next"), "commande inattendue: {ligne}");

        for _ in 0..100 {
            if recus_test.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("l'etat n'a pas atteint l'afficheur alors que l'entree fonctionnait");
    }
}
