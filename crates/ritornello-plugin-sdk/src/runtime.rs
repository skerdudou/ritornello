//! Constructeur d'un greffon : on enregistre une moitié par kind, chacune
//! liant son socket immédiatement, puis `run()` announcement et sert.
//!
//! L'order « lier d'abord, annoncer ensuite » n'est pas une consigne mais une
//! propriété de ce type : les méthodes lient, seul `run()` écrit l'announcement. Un
//! greffon ne peut donc pas annoncer un kind dont le socket n'est pas prêt.

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

/// Une moitié prête à serve : son kind, pour l'announcement, et sa boucle.
struct Half {
    kind: PluginKind,
    /// Cette moitié veut-elle les bytes des pochettes ? Toujours `false` hors
    /// d'un afficheur.
    ///
    /// Retenu **ici**, dans le registre des moitiés, et non dans un champ à
    /// part du `Runtime` : c'est ce qui fait de `covers` une valeur *dérivée*
    /// de ce qui a été enregistré, exactement comme `kinds` et `admin` —
    /// `run()` les calcule tous les trois depuis ce registre, en une
    /// expression chacun, et l'announcement ne peut donc pas décrire autre chose
    /// que ce qui sert réellement. La valeur est lue par `display()`, avant que
    /// le plugin ne soit déplacé dans sa boucle : après, il n'est plus
    /// interrogeable.
    covers: bool,
    serve: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}

pub struct Runtime {
    name: String,
    register: PathBuf,
    prefix: PathBuf,
    halves: Vec<Half>,
    /// La boucle de la page d'admin, si `.admin()` a été appelé. Hors de
    /// `halves` : `admin` n'est pas un `PluginKind`, c'est un drapeau de
    /// l'announcement.
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
        // Lu **avant** le déplacement dans `serve_display`, seul order
        // possible : après, le plugin appartient à la future qui le sert et
        // plus personne ne peut l'interroger. C'est aussi ce qui rend le
        // drapeau impossible à falsifier — il n'y a pas de paramètre à
        // renseigner, seulement une méthode du plugin à read.
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

    /// Annonce, puis sert toutes les moitiés jusqu'à ce que l'une s'arrête.
    ///
    /// Chaque moitié tourne dans sa propre tâche : la panne de la page
    /// d'admin ne doit pas couper l'audio, et réciproquement — c'est
    /// exactement ce que les plugins `radio`, `files` et `generic-input`
    /// faisaient à la main avant ce constructeur.
    pub async fn run(self) -> Result<()> {
        let announcement = Announcement {
            name: self.name.clone(),
            kinds: self.halves.iter().map(|m| m.kind).collect(),
            admin: self.admin.is_some(),
            // Dérivé, comme les deux au-dessus : la seule source est le
            // registre des moitiés, donc aucun path ne peut annoncer des
            // pochettes pour un afficheur qui n'en veut pas, ni l'inverse.
            covers: self.halves.iter().any(|m| m.covers),
        };
        let mut stream = UnixStream::connect(&self.register)
            .await
            .with_context(|| format!("connecting to {}", self.register.display()))?;
        stream.write_all(format!("{}\n", serde_json::to_string(&announcement)?).as_bytes()).await?;
        stream.shutdown().await?;
        drop(stream);
        tracing::info!("announced as {} ({:?})", announcement.name, announcement.kinds);

        // Chaque moitié est suivie **indépendamment jusqu'au bout**. Surtout
        // pas de `select_all` ni de `try_join!` : la première moitié qui rend
        // la main — même proprement — terminerait alors tout le greffon, et
        // les autres tâches seraient abandonnées sans que leur échec soit
        // jamais observé. C'est exactement ce que l'ancien montage à la main
        // de `generic-input` interdisait, avec un commentaire qui proscrivait
        // déjà `try_join!` en toutes lettres.
        //
        // `FuturesUnordered` donne le meilleur des deux : chaque moitié est
        // journalisée **dès** qu'elle se terminate, en étant nommée, sans que
        // cela cesse d'attendre les autres.
        let mut taches = Vec::new();
        for m in self.halves {
            let name = format!("{:?}", m.kind).to_lowercase();
            taches.push((name, tokio::spawn(m.serve)));
        }
        if let Some(admin) = self.admin {
            taches.push(("admin".to_string(), tokio::spawn(admin)));
        }

        let mut en_cours: futures::stream::FuturesUnordered<_> = taches
            .into_iter()
            .map(|(name, tache)| async move { (name, tache.await) })
            .collect();

        let mut echecs = 0usize;
        while let Some((name, resultat)) = en_cours.next().await {
            match resultat {
                Ok(Ok(())) => tracing::info!("{name} half ended"),
                Ok(Err(e)) => {
                    echecs += 1;
                    tracing::error!("{name} half failed: {e:#}");
                }
                // Une panique est capturée dans le `JoinHandle` au lieu de
                // dérouler la pile de l'autre moitié.
                Err(e) => {
                    echecs += 1;
                    tracing::error!("{name} half panicked: {e}");
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

    /// Un afficheur qui **redéfinit** `wants_covers`, et rien d'autre. Le seul
    /// écart avec `AfficheurBouchon` est cette méthode, donc c'est bien elle
    /// que l'announcement doit refléter.
    struct AfficheurQuiVeutLesPochettes;

    #[async_trait::async_trait]
    impl crate::DisplayPlugin for AfficheurQuiVeutLesPochettes {
        async fn show(&mut self, _state: PlayerState) -> anyhow::Result<()> {
            Ok(())
        }
        fn wants_covers(&self) -> bool {
            true
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

    /// Lit l'unique announcement déposée sur un socket d'enregistrement.
    async fn lire_annonce(listener: &UnixListener) -> Announcement {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let line = lines.next_line().await.unwrap().expect("une announcement");
        serde_json::from_str(&line).unwrap()
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
        assert!(!a.covers, "aucun afficheur n'a redefini wants_covers");
    }

    /// L'invariant sur lequel repose tout le protocol d'enregistrement :
    /// l'announcement est **dérivée** de ce qui a été enregistré, elle ne peut donc
    /// pas mentir. Éprouvé dans les deux sens sur le seul drapeau que ce
    /// chantier add, avec deux afficheurs qui ne diffèrent *que* par
    /// `wants_covers`.
    ///
    /// Le sens négatif est celui qui protège la console : un afficheur de vingt
    /// colonnes n'a redéfini rien, et le cœur ne doit donc jamais lui pousser
    /// de mégaoctets.
    #[tokio::test]
    async fn le_drapeau_des_pochettes_est_derive_de_lafficheur_enregistre() {
        for (veut, plugin) in [(false, 0u8), (true, 1u8)] {
            let dir = tempfile::tempdir().unwrap();
            let register = dir.path().join("register.sock");
            let listener = UnixListener::bind(&register).unwrap();
            let prefixe = dir.path().join("afficheur");

            let rt = Runtime::new("afficheur".into(), register.clone(), prefixe.clone());
            let rt = if plugin == 0 {
                // Ne redéfinit pas `wants_covers` : le corps par défaut décide.
                rt.display(AfficheurBouchon { recus: Arc::new(Mutex::new(Vec::new())) }).unwrap()
            } else {
                rt.display(AfficheurQuiVeutLesPochettes).unwrap()
            };
            tokio::spawn(async move { rt.run().await.unwrap() });

            let a = lire_annonce(&listener).await;
            assert_eq!(a.kinds, vec![PluginKind::Display]);
            assert_eq!(
                a.covers, veut,
                "l'announcement doit decrire exactement ce que l'afficheur enregistre veut"
            );
        }
    }

    /// Un kind sans afficheur ne peut pas annoncer de pochettes : le drapeau
    /// est calculé depuis le registre des moitiés, où seul un afficheur peut
    /// poser `covers: true`.
    #[tokio::test]
    async fn un_greffon_sans_afficheur_nannonce_jamais_de_pochettes() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let prefixe = dir.path().join("entree");

        let (_tx, rx) = tokio::sync::mpsc::channel(4);
        let rt = Runtime::new("entree".into(), register.clone(), prefixe.clone())
            .input(EntreeBouchon { rx })
            .unwrap();
        tokio::spawn(async move { rt.run().await.unwrap() });

        let a = lire_annonce(&listener).await;
        assert_eq!(a.kinds, vec![PluginKind::Input]);
        assert!(!a.covers);
    }

    #[tokio::test]
    async fn les_sockets_sont_lies_avant_que_lannonce_soit_lisible() {
        // C'est l'invariant central du chantier : quand le coeur read
        // l'announcement, il peut se connecter sans retenter.
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
        for kind in a.kinds {
            let path = crate::socket_kind(&prefixe, kind);
            UnixStream::connect(&path)
                .await
                .unwrap_or_else(|e| panic!("{} refuse la connexion: {e}", path.display()));
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

        // Cote afficheur : le coeur push_cover un state.
        let display = UnixStream::connect(crate::socket_kind(&prefixe, PluginKind::Display))
            .await
            .unwrap();
        let (_r, mut w) = display.into_split();
        let trame = ritornello_proto::DisplayFrame::State(PlayerState::default());
        w.write_all(format!("{}\n", serde_json::to_string(&trame).unwrap()).as_bytes())
            .await
            .unwrap();

        // Cote entree : le greffon push_cover une commande.
        let input = UnixStream::connect(crate::socket_kind(&prefixe, PluginKind::Input))
            .await
            .unwrap();
        tx.send(ritornello_proto::Command::Next.into()).await.unwrap();
        let mut lines = BufReader::new(input).lines();
        let line = lines.next_line().await.unwrap().expect("une commande");
        assert!(line.contains("Next"), "commande inattendue: {line}");

        for _ in 0..100 {
            if recus_test.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("l'state n'a pas atteint l'afficheur alors que l'entree fonctionnait");
    }
}
