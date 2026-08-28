//! Source `files` : lit des fichiers audio depuis une racine locale ou un
//! partage réseau monté.
//!
//! mpv tient la liste de lecture : le plugin lui donne un m3u généré et pilote
//! l'index. L'avance automatique passe donc par `playlist-pos`, exactement
//! comme pour un disque, et le plugin n'a rien à cadencer lui-même.
//!
//! Deux moitiés indépendantes, sur le plan du plugin radio : la Source et la
//! page d'admin, chacune dans sa tâche, partageant la table des racines et la
//! liste en cours. Une panne de la page ne doit jamais couper l'audio.

mod admin;
mod pochette;
mod state;

use anyhow::Result;
use ritornello_i18n::Catalog;
use ritornello_plugin_files::m3u::Entry;
use ritornello_plugin_files::playlist::Playlist;
use ritornello_plugin_files::roots::Roots;
use ritornello_plugin_files::FILES_EN;
use ritornello_plugin_sdk::{Notification, Runtime, SourceOutcome, SourcePlugin};
use ritornello_proto::{Preset, SourceAction};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::RwLock as AsyncRwLock;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct FilesSource {
    /// Partagée avec la moitié Admin, qui la modifie depuis la page.
    playlist: Arc<AsyncRwLock<Playlist>>,
    /// La page a modifié la liste depuis qu'on l'a confiée à mpv.
    ///
    /// mpv joue une **copie**, écrite au dernier `Play`. Toute modification l'en
    /// écarte, et la moitié Admin ne peut rien lui dire : les notifications du
    /// SDK sont volontairement sans action. Ce drapeau est donc le seul canal, et
    /// on s'en sert au prochain ordre reçu — c'est là qu'on peut légitimement
    /// rendre à mpv une liste neuve.
    liste_changee: Arc<std::sync::atomic::AtomicBool>,
    /// Joue-t-on en ce moment. Lu par la page (voir `joue` côté Admin).
    joue: Arc<std::sync::atomic::AtomicBool>,
    state_path: PathBuf,
    /// Le m3u **généré** que mpv reçoit. Découplé de toute liste utilisateur.
    mpv_playlist_path: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    locales_root: PathBuf,
    /// Compte de présélections annoncé par la moitié Admin après chaque
    /// modification de la liste.
    ///
    /// `main()` construit toujours ce champ à `Some` : la page d'admin est
    /// enregistrée sans condition auprès de `Runtime`. `None` n'apparaît que
    /// dans les tests, qui construisent `FilesSource` directement sans passer
    /// par `Runtime` et donc sans moitié Admin pour émettre sur ce canal ;
    /// `poll_notification` reste alors en attente pour toujours plutôt que de
    /// rendre `None`, qui est **terminal** pour le SDK.
    preset_count_rx: Option<tokio::sync::watch::Receiver<u8>>,
    /// Résultat en vol de la recherche de pochette pour la piste armée.
    ///
    /// **Portée par une tâche `tokio::spawn` indépendante**, lancée par
    /// `arme_pochette` — et non par un appel direct à `sante.borne(...).await`
    /// ici.
    /// Une première version faisait cet appel directement dans
    /// `poll_notification`, qui est le futur que le `select!` du SDK annule
    /// dès qu'une requête du cœur arrive pendant l'attente — un événement
    /// courant, pas un cas limite. Une annulation avant l'expiration du délai
    /// de `sante` fait sauter son bras `Err` : rien n'est marqué muet, la
    /// tâche `spawn_blocking` interne est simplement détachée (Tokio n'annule
    /// rien à la destruction), et comme la piste armée n'est délibérément pas
    /// oubliée sur annulation, l'appel suivant relançait une **deuxième**
    /// sonde sur le même partage bloqué — un fil `spawn_blocking` de plus à
    /// chaque cycle, là où `sante.rs` promet au plus un fil abandonné par
    /// point de montage. En sortant la sonde de cette boucle annulable, elle
    /// va toujours à son terme une seule fois, et la comptabilité de `sante`
    /// reste exacte.
    ///
    /// `oneshot::Receiver::await` est documenté cancel-safe : si
    /// `poll_notification` est annulé pendant l'attente, ce récepteur — gardé
    /// ici et non dans une variable locale du futur — n'a rien perdu, et le
    /// prochain appel reprend l'attente sur la même tâche en vol plutôt que
    /// d'en relancer une autre.
    ///
    /// Un nouveau `Play` pendant qu'une sonde est en vol remplace ce champ par
    /// un récepteur neuf : l'ancien est abandonné, et le résultat de l'ancienne
    /// tâche — quand elle finira par arriver — tombera dans un `send` sans
    /// personne à l'écoute. C'est délibéré : une pochette de la piste
    /// précédente ne doit jamais s'annoncer pour la piste qui vient de
    /// démarrer.
    pochette_en_vol: Option<tokio::sync::oneshot::Receiver<Option<ritornello_proto::CoverRef>>>,
    /// Pochette mémorisée **par répertoire** : le répertoire sondé, et ce
    /// qu'on y a trouvé (`None` en second = sondé, rien de sûr).
    ///
    /// Le répertoire et non le fichier, parce que c'est la granularité de la
    /// chose cherchée : un `folder.jpg` appartient à l'album, pas à la piste.
    /// C'est ce qui permet de réannoncer la pochette à **chaque** déclaration
    /// d'identité — l'avance automatique de mpv comprise, qui passe par
    /// `player_track`/`recale` et non par `jouer()` — sans repayer un
    /// `readdir` sur un partage SMB à chaque piste. Sans cette réannonce, un
    /// album ripé montrait sa pochette sur la piste 1 et le repli ♫ sur les
    /// suivantes : le cœur efface `cover_source` à tout changement d'identité
    /// (voir `Metadonnees::set_identity`), et seul `jouer()` réarmait la
    /// sonde.
    ///
    /// Partagée avec la tâche de sonde, qui l'écrit en fin de course, d'où
    /// l'`Arc<Mutex<…>>`. Un seul répertoire mémorisé : on n'en écoute qu'un à
    /// la fois, et revenir en arrière dans la liste ne coûte qu'un `readdir`.
    // Type volontairement laissé tel que le chantier des pochettes l'a écrit.
    // `clippy::type_complexity` le refuse, et un alias nommé aurait été le
    // correctif que la règle suggère — mais le nommer obligeait à en documenter
    // le sens, donc à interpréter la sémantique du double `Option` d'un autre
    // chantier depuis un commit de fusion qu'il n'a pas relu. Une affirmation
    // erronée posée à côté du code d'autrui est pire qu'une règle tue : la
    // règle, elle, est honnête sur ce qu'elle est, alors que le commentaire se
    // lit comme du savoir. La doc du champ ci-dessus est la leur, et elle
    // suffit.
    #[allow(clippy::type_complexity)]
    pochette_par_repertoire: Arc<Mutex<Option<(PathBuf, Option<ritornello_proto::CoverRef>)>>>,
    /// Disjoncteur des chemins média, partagé avec la moitié Admin.
    ///
    /// Le `read_dir` de la recherche de pochette porte sur un partage qui peut
    /// rester muet indéfiniment (voir `sante`) : sans cette borne, un NAS
    /// endormi figerait la tâche de sonde ci-dessus indéfiniment.
    sante: Arc<ritornello_plugin_files::sante::Sante>,
}

impl FilesSource {
    /// Identité de ce qui joue : le fichier, désigné par son chemin absolu.
    ///
    /// Opaque pour le cœur, qui ne fait que la comparer et la relayer. C'est
    /// aussi ce qu'un plugin `metadata` lirait pour reconnaître un morceau.
    fn identite(path: &Path) -> serde_json::Value {
        serde_json::json!({ "kind": "file", "path": path.to_string_lossy() })
    }

    fn mot(&self, cle: &str) -> String {
        self.catalog.read().unwrap().get(cle).to_string()
    }

    /// Statut permanent de la source.
    ///
    /// **Redéclaré à chaque trame utile** : `status` a la convention inverse de
    /// `preset`, l'absence voulant dire « pas de statut » et non « garde le
    /// précédent ». Une Source qui l'omettrait verrait son affichage s'effacer
    /// tout seul à la trame suivante.
    fn statut(&self) -> String {
        self.mot("status_files")
    }

    async fn persiste(&self) {
        let index = self.playlist.read().await.index;
        // `update` et non `save` : la moitié Admin écrit la liste dans ce même
        // fichier, et un `save` reconstruit ici l'effacerait. L'échec est
        // journalisé et non propagé — un `/var/lib` en lecture seule doit
        // coûter la reprise après redémarrage, pas la lecture en cours.
        if let Err(e) = state::update(&self.state_path, |s| s.index = index) {
            tracing::warn!("persisting the current track: {e}");
        }
    }

    /// Arme l'annonce de la pochette du répertoire de `fichier`.
    ///
    /// À appeler depuis **tout** chemin qui déclare une identité : le cœur
    /// remet sa pochette à zéro à chaque changement d'identité, donc une
    /// identité déclarée sans réannonce est une pochette perdue.
    ///
    /// Deux cas, et c'est là tout l'intérêt de la mémorisation :
    /// - le répertoire est celui qu'on a déjà sondé — cas de l'immense
    ///   majorité des changements de piste, un album étant un répertoire — et
    ///   la réponse part **tout de suite**, sans aucun accès disque ;
    /// - le répertoire change : on sonde, une fois.
    ///
    /// La sonde reste portée par une tâche `tokio::spawn` indépendante avec un
    /// `oneshot`, et ce n'est pas un détail de style (voir la doc de
    /// `pochette_en_vol`) : le `select!` du SDK annule `poll_notification` dès
    /// qu'une requête du cœur arrive, et un appel à `sante.borne(...)` fait
    /// depuis ce futur perdrait la comptabilité du disjoncteur. Le chemin
    /// mémorisé passe par le même `oneshot`, déjà rempli : rien de neuf à
    /// annuler, et surtout **aucun chemin par lequel `poll_notification`
    /// pourrait rendre `None`**, qui est terminal pour le SDK — un `Err` du
    /// récepteur comme un `Ok(None)` retombent tous deux dans la suite de la
    /// fonction.
    fn arme_pochette(&mut self, fichier: &Path) {
        // Un récepteur neuf remplace celui d'une sonde encore en vol : c'est
        // ce qui écarte la pochette d'une piste déjà quittée (voir la doc de
        // `pochette_en_vol`).
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pochette_en_vol = Some(rx);
        let Some(repertoire) = fichier.parent().map(Path::to_path_buf) else {
            let _ = tx.send(None);
            return;
        };
        if let Some((connu, trouvee)) = &*self.pochette_par_repertoire.lock().unwrap() {
            if connu == &repertoire {
                // En `debug` et non en `info` : `arme_pochette` est appelee
                // deux fois par piste (la lecture, puis le recalage), donc
                // cette ligne sortait deux fois **par piste** pour un fait qui
                // ne change pas de tout l'album. Le releve frais ci-dessous,
                // lui, reste en `info` : une fois par repertoire, c'est la
                // reponse utile a « pourquoi pas de pochette ».
                if trouvee.is_none() {
                    tracing::debug!("no cover file in {} (remembered)", repertoire.display());
                }
                let _ = tx.send(trouvee.clone());
                return;
            }
        }
        let sante = self.sante.clone();
        let memoire = self.pochette_par_repertoire.clone();
        let chemin = fichier.to_path_buf();
        tokio::spawn(async move {
            let a_chercher = chemin.clone();
            match sante.borne(&chemin, move || pochette::cherche(&a_chercher)).await {
                Some(trouve) => {
                    match &trouve {
                        Some(_) => tracing::info!("cover file found in {}", repertoire.display()),
                        None => tracing::info!("no cover file in {}", repertoire.display()),
                    }
                    // Mémorisé y compris quand rien n'a été trouvé : c'est ce
                    // qui évite de re-sonder un répertoire sans image à
                    // chaque piste.
                    *memoire.lock().unwrap() = Some((repertoire, trouve.clone()));
                    let _ = tx.send(trouve);
                }
                // Le disjoncteur n'a pas su (partage muet, délai écoulé) :
                // **rien n'est mémorisé**. Retenir « pas de pochette » ici
                // condamnerait ce répertoire pour toute la session sur la
                // seule foi d'un NAS momentanément endormi, alors que `sante`
                // rend justement la main dès qu'il répond de nouveau.
                None => {
                    // Incident reel — c'est le partage muet que `sante` existe
                    // pour borner — donc `warn`, et non le silence d'avant.
                    tracing::warn!("cover lookup in {} gave up: share not answering", repertoire.display());
                    let _ = tx.send(None);
                }
            }
            // Ignoré si personne n'écoute plus (piste déjà changée depuis) :
            // c'est le mécanisme même qui écarte un résultat périmé.
        });
    }

    /// Lance la liste à l'index courant, après avoir réécrit le m3u de mpv.
    async fn jouer(&mut self) -> SourceOutcome {
        // On rend à mpv la liste telle qu'elle est maintenant : l'écart est
        // refermé, quelle qu'en fût la cause.
        self.liste_changee.store(false, std::sync::atomic::Ordering::Relaxed);
        let liste = self.playlist.read().await;
        let count = liste.preset_count();
        let Some(entry) = liste.current().cloned() else {
            self.joue.store(false, std::sync::atomic::Ordering::Relaxed);
            return SourceOutcome::new(SourceAction::Noop)
                .status(self.mot("no_playlist"))
                .preset_count(0)
                .plays_nothing();
        };
        self.joue.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = liste.write_for_mpv(&self.mpv_playlist_path) {
            tracing::warn!("writing the mpv playlist: {e}");
        }
        let index = liste.index;
        let preset = liste.preset();
        drop(liste);
        // Armée ici, lue plus tard par `poll_notification`. Un `Play` réel
        // seulement — la liste vide est sortie plus haut par le `return`, donc
        // on ne sonde jamais pour rien.
        self.arme_pochette(&entry.path);

        let action = SourceAction::play(self.mpv_playlist_path.to_string_lossy().to_string())
            // Sans cette déclaration, le cœur chargerait le m3u comme un média
            // unique : mpv ne le déplierait qu'après coup, l'index de départ
            // arriverait hors bornes, et toute sélection de piste rejouerait la
            // première en perdant l'affichage. Mesuré, et corrigé ici.
            .playlist()
            .starting_at(index as i64)
            // Une liste de fichiers a une fin normale : sans cette
            // déclaration, l'inactivité de mpv en fin de liste passerait pour
            // une coupure de flux et la relance rejouerait la liste en boucle.
            .finite();
        let mut issue = SourceOutcome::new(action)
            .plays(Self::identite(&entry.path))
            .preset_name(entry.display_name())
            .preset_count(count)
            .status(self.statut());
        if let Some(n) = preset {
            issue = issue.preset(n);
        }
        issue
    }

    /// Si la page a modifié la liste, la rend à mpv en se décalant de `pas`.
    ///
    /// `None` quand rien n'a changé : l'appelant délègue alors à mpv, comme
    /// avant. Sans ce recalage, suivant/précédent marchaient dans la liste que
    /// mpv tenait au dernier `Play` — les pistes ajoutées depuis étaient hors
    /// d'atteinte, et celles retirées revenaient.
    ///
    /// Le décalage part de **notre** index, que la moitié Admin maintient à jour
    /// au fil de ses modifications ; celui de mpv, lui, désigne une position dans
    /// une liste périmée.
    async fn recharger_si_changee(&mut self, pas: i64) -> Option<SourceOutcome> {
        if !self.liste_changee.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        {
            let mut liste = self.playlist.write().await;
            if liste.entries.is_empty() {
                // Rien à jouer : `jouer()` le dira, et il n'y a pas d'index à
                // déplacer.
                drop(liste);
                return Some(self.jouer().await);
            }
            let n = liste.entries.len() as i64;
            // Boucle sur les bornes, comme le fait mpv d'un bout à l'autre de sa
            // propre liste : l'utilisateur ne doit pas se retrouver bloqué parce
            // qu'une modification l'a laissé sur la dernière piste.
            liste.index = (((liste.index as i64 + pas) % n + n) % n) as usize;
        }
        Some(self.jouer().await)
    }

    /// Trame qui ne fait que redire où on en est, sans rien relancer.
    ///
    /// « Sans rien relancer » côté audio ; côté pochette, au contraire, elle
    /// **réannonce**. Cette trame déclare une identité, et le cœur efface ce
    /// qu'il tenait à chaque changement d'identité : c'est le chemin de
    /// l'avance automatique de mpv, donc de toutes les pistes d'un album sauf
    /// celle que l'utilisateur a lancée lui-même. La sonde n'est repayée que
    /// si le répertoire change (voir `arme_pochette`).
    async fn recale(&mut self) -> SourceOutcome {
        let liste = self.playlist.read().await;
        let mut issue = SourceOutcome::new(SourceAction::Noop)
            .preset_count(liste.preset_count())
            .status(self.statut());
        let mut fichier = None;
        if let Some(entry) = liste.current() {
            issue = issue.plays(Self::identite(&entry.path)).preset_name(entry.display_name());
            fichier = Some(entry.path.clone());
        }
        if let Some(n) = liste.preset() {
            issue = issue.preset(n);
        }
        drop(liste);
        if let Some(fichier) = fichier {
            self.arme_pochette(&fichier);
        }
        issue
    }
}

#[async_trait::async_trait]
impl SourcePlugin for FilesSource {
    async fn activate(&mut self) -> SourceOutcome {
        // L'index est conservé : reprendre après un arrêt rend la piste qu'on
        // écoutait, et non la première.
        //
        // Une version antérieure repartait du début quand la liste s'était
        // terminée, en se fiant au `playlist-pos = -1` de mpv. Mesuré : ce -1
        // arrive **aussi de façon transitoire à chaque rechargement de liste**,
        // donc à chaque changement de piste. La reprise retombait alors sur la
        // piste 1. Le signal n'étant pas fiable, la distinction est abandonnée
        // plutôt que devinée — au prix d'un détail : après une liste allée à son
        // terme, la touche Lecture rejoue la dernière piste.
        self.jouer().await
    }

    async fn deactivate(&mut self) -> SourceOutcome {
        self.joue.store(false, std::sync::atomic::Ordering::Relaxed);
        SourceOutcome::new(SourceAction::Stop).plays_nothing().status(self.statut())
    }

    async fn select(&mut self, n: u8) -> SourceOutcome {
        if self.playlist.write().await.select(n) {
            self.persiste().await;
            return self.jouer().await;
        }
        // Rien n'a été lancé : la piste précédente joue toujours. Message
        // éphémère, et surtout **aucune déclaration d'identité** — un
        // `plays_nothing()` ici ferait cesser les plugins `metadata` et
        // viderait le titre affiché alors que le son continue.
        let compte = self.playlist.read().await.preset_count();
        SourceOutcome::new(SourceAction::Noop)
            .status(self.mot("empty_track"))
            .transient()
            .preset_count(compte)
    }

    async fn next(&mut self) -> SourceOutcome {
        // La liste a changé sous mpv : lui rendre la nouvelle, positionnée sur
        // la piste qui suit. C'est le moment légitime pour le faire — un ordre
        // explicite de l'utilisateur, qui s'attend à un changement de piste.
        if let Some(issue) = self.recharger_si_changee(1).await {
            return issue;
        }
        // Sinon mpv marche dans sa propre liste ; c'est lui qui nous dira où il
        // est arrivé, par `player_track`. Rien à recaler ici, sous peine de le
        // faire deux fois et de se contredire.
        SourceOutcome::new(SourceAction::PlayerNext).status(self.statut())
    }

    async fn prev(&mut self) -> SourceOutcome {
        if let Some(issue) = self.recharger_si_changee(-1).await {
            return issue;
        }
        SourceOutcome::new(SourceAction::PlayerPrev).status(self.statut())
    }

    async fn eject(&mut self) -> SourceOutcome {
        // Rien à éjecter : pas de support amovible ici.
        SourceOutcome::new(SourceAction::Noop).status(self.statut())
    }

    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        // mpv vient de passer à la piste suivante **de lui-même**. Si la liste a
        // changé depuis qu'il l'a reçue, c'est le meilleur moment pour lui rendre
        // la nouvelle : il démarre un fichier de toute façon, donc rien n'est
        // interrompu — là où attendre un ordre explicite laissait la lecture
        // enchaîner dans l'ancienne liste, et c'est exactement ce que l'usage a
        // montré comme « les modifications ne font rien ».
        //
        // Seulement pour un index valide : à `-1` la liste est terminée, et
        // recharger la relancerait au lieu de la laisser finir.
        if n >= 0 {
            // Le décalage part de **notre** index — la piste qui vient de
            // s'achever — donc « la suivante » se lit dans la liste à jour.
            if let Some(issue) = self.recharger_si_changee(1).await {
                return issue;
            }
        }
        if !self.playlist.write().await.set_index(n) {
            // mpv dit `-1` en fin de liste — **et aussi transitoirement à chaque
            // rechargement de liste**, donc à chaque changement de piste : c'est
            // mesuré, et c'est pourquoi on n'en tire aucune conclusion. Ne rien
            // déclarer ; l'arrêt éventuel sera annoncé par `stop()`.
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.persiste().await;
        self.recale().await
    }

    async fn stop(&mut self) -> SourceOutcome {
        // Le cœur a arrêté de sa propre initiative, ou la liste s'est terminée.
        // Le dire, sinon la dernière piste et ses métadonnées resteraient
        // affichées indéfiniment.
        //
        // Et **dire lequel des trois**, ce qui n'était pas le cas : cette trame
        // écrasait le « AUCUNE LISTE » que `jouer()` venait d'afficher. Sans
        // piste, mpv reste inactif, le cœur envoie donc `stop()` aussitôt, et
        // l'utilisateur ne voyait qu'un statut générique sans jamais apprendre
        // que sa liste était vide.
        self.joue.store(false, std::sync::atomic::Ordering::Relaxed);
        let liste = self.playlist.read().await;
        if liste.entries.is_empty() {
            return SourceOutcome::new(SourceAction::Noop)
                .plays_nothing()
                .status(self.mot("no_playlist"))
                .preset_count(0);
        }
        // **Arrêté, mais une piste armée.** L'ancienne trame n'annonçait qu'un
        // statut : l'afficheur perdait numéro et nom, et ne montrait plus que
        // « FILES » sans qu'on sache où on en était. Déclarer la piste courante
        // sans identité de lecture dit exactement l'état réel — rien ne joue, et
        // voilà ce qui repartira.
        let mut issue = SourceOutcome::new(SourceAction::Noop)
            .plays_nothing()
            .status(self.statut())
            .preset_count(liste.preset_count());
        if let Some(entry) = liste.current() {
            issue = issue.preset_name(entry.display_name());
        }
        if let Some(n) = liste.preset() {
            issue = issue.preset(n);
        }
        issue
    }

    async fn set_locale(&mut self, locale: String) {
        *self.catalog.write().unwrap() =
            Catalog::load("files", &locale, &self.locales_root, FILES_EN);
    }

    /// Les présélections nommées, pour la grille de la page d'accueil et pour
    /// le catalogue que le cœur tient à l'usage des afficheurs.
    ///
    /// Sans cette surcharge, le corps par défaut du trait rendait une liste
    /// vide : la source ne déclarait qu'un `preset_count`, et les tuiles de la
    /// grille ne portaient qu'un numéro là où la radio affiche « 1 · FIP ».
    /// C'est la même voie que la radio emprunte, et le catalogue distingue déjà
    /// « je n'ai que des numéros » (liste vide) de « voici mes noms ».
    async fn list_presets(&mut self) -> Vec<Preset> {
        self.playlist.read().await.presets()
    }

    async fn poll_notification(&mut self) -> Option<Notification> {
        // Une sonde est en vol : attendre son résultat, sans jamais la
        // relancer depuis ce futur (voir la doc du champ — c'est `jouer()`
        // qui la lance, sur une tâche que cette annulation-ci ne touche pas).
        //
        // `rx.await` et non `rx.try_recv()` : c'est justement l'attente
        // elle-même qui doit survivre à l'annulation de `poll_notification`,
        // pas la contourner. `oneshot::Receiver` documente son `.await` comme
        // cancel-safe, et le récepteur vit dans `self` — pas dans une
        // variable locale de ce futur — donc rien n'est perdu si ce tour est
        // interrompu : le prochain reprend l'attente sur la même tâche.
        if let Some(rx) = &mut self.pochette_en_vol {
            let resultat = rx.await;
            // Vidé seulement après que la sonde a répondu — c'est ce qui rend
            // vraie la garantie ci-dessus : tant qu'aucune réponse n'est
            // arrivée, le champ reste en place pour le prochain tour.
            self.pochette_en_vol = None;
            // Deux échecs distincts se rejoignent ici sans faire de
            // différence : `Err` (la tâche a disparu sans répondre, par
            // exemple si elle a paniqué) et un `Ok(None)` (le disjoncteur a
            // dit « on ne sait pas », ou la recherche elle-même a dit « rien
            // de sûr »). Dans tous les cas, il n'y a rien à annoncer —
            // surtout pas une notification vide, et surtout pas `None`, qui
            // est terminal pour le SDK (voir le commentaire sur
            // `preset_count_rx` juste en dessous). On tombe simplement dans
            // la suite de la fonction, qui attend le prochain événement.
            if let Ok(Some(cover)) = resultat {
                return Some(Notification::new().cover(cover));
            }
        }
        let Some(rx) = &mut self.preset_count_rx else {
            // N'arrive qu'en test (voir le commentaire sur le champ) : `main()`
            // construit toujours ce récepteur. Jamais `None` ici, qui serait
            // terminal pour le SDK.
            return std::future::pending().await;
        };
        match rx.changed().await {
            Ok(()) => {
                let n = *rx.borrow_and_update();
                // Le compte, **et le numéro et le nom de la piste courante**.
                //
                // Le numéro seul ne suffisait pas : réordonner la liste change la
                // position de ce qu'on écoute, et le compteur de l'afficheur
                // restait sur l'ancienne — la page du plugin était juste, le
                // lecteur non. C'est le pendant exact du correctif de la radio,
                // où la présélection est aussi une position.
                //
                // Toujours **aucune identité et aucune action** : la piste en
                // cours ne doit être ni interrompue ni redéclarée, seulement
                // renumérotée.
                //
                // Attention : le cœur **ne fusionne pas** `status`, contrairement
                // à ce que cette place affirmait. `preset`, `preset_name` et
                // `preset_count` sont bien conservés quand ils sont absents —
                // c'est ce qui rend cet avis partiel légitime — mais `status`,
                // lui, est *remplacé* par ce que porte la trame, absence
                // comprise (`Core::handle_source_update` : `if !update.transient
                // { self.source_status = update.status.clone(); }`). C'est la
                // seule convention qui permette d'effacer un statut.
                //
                // Cet avis n'en déclare donc aucun **et n'en efface aucun** : le
                // cœur rend la main avant ce traitement pour une trame qui ne
                // déclare ni identité ni statut. Sans ce garde — et c'était le
                // cas en service — enregistrer une liste depuis cette page
                // blanchissait le statut de la source sur la console et la SPA
                // jusqu'à la commande suivante.
                let liste = self.playlist.read().await;
                let mut avis = Notification::new().preset_count(n);
                // Les **noms**, republiés avec le compte. Le canal se réveille à
                // chaque modification de la liste (`watch::send` signale même à
                // valeur égale), donc un simple réordonnancement — qui ne change
                // pas le compte — renomme quand même les tuiles. Sans cela, la
                // grille aurait gardé les titres d'avant sous les nouveaux
                // numéros, ce qui est pire qu'aucun titre.
                //
                // Rien n'est publié pour une liste vide : c'est l'absence qui
                // dit « je n'ai que des numéros » (voir `SourceOutcome::presets`),
                // et une liste vide y serait indistinguable d'un effacement
                // volontaire du catalogue.
                let presets = liste.presets();
                if !presets.is_empty() {
                    avis = avis.presets(presets);
                }
                if let Some(entry) = liste.current() {
                    avis = avis.preset_name(entry.display_name());
                }
                if let Some(p) = liste.preset() {
                    avis = avis.preset(p);
                }
                Some(avis)
            }
            // L'émetteur a disparu (moitié Admin terminée) : plus rien à
            // annoncer, mais la Source continue de jouer.
            Err(_) => std::future::pending().await,
        }
    }
}

/// Vrai pour une trame que ce greffon accepte d'écrire au journal.
///
/// **Elle n'écarte qu'une chose : le bavardage de `lofty` sous le niveau
/// erreur.** Le relevé des durées ouvre l'en-tête de chaque fichier de la
/// liste, et `lofty` y émet un `WARN` par MP3 sans en-tête Xing —
/// « MPEG: Using bitrate to estimate duration ». Ce n'est pas un incident :
/// c'est la méthode d'estimation normale pour ce format, elle n'appelle
/// aucune action, et elle se répète par piste. Signalée par le propriétaire
/// comme polluant son journal, et le coût est réel : le cœur ne retient que
/// les lignes `WARN` et au-delà pour la carte « dernières erreurs », donc ce
/// bruit-là chasse de vraies erreurs du tampon.
///
/// `lofty` garde ses `ERROR` : une trame que la bibliothèque juge fautive
/// reste une information.
///
/// Un `filter_fn` et non un `EnvFilter` : ce dernier vit derrière la fonction
/// optionnelle `env-filter` de `tracing-subscriber`, qui tire `regex` — une
/// dépendance de plus à compiler et à embarquer sur un Pi, pour une seule
/// règle connue à l'avance.
fn trame_a_journaliser(metadata: &tracing::Metadata<'_>) -> bool {
    // `>` et non `<` : dans `tracing`, l'ordre des niveaux est celui de la
    // verbosité, donc `ERROR` est le plus **petit**. « Plus verbeux qu'erreur »
    // s'écrit bien `> Level::ERROR`.
    !(metadata.target().starts_with("lofty") && *metadata.level() > tracing::Level::ERROR)
}

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .with(tracing_subscriber::filter::filter_fn(trame_a_journaliser))
        .init();

    let state_path =
        PathBuf::from(env_or("RITORNELLO_FILES_STATE", "/var/lib/ritornello/plugin-files.json"));
    let mpv_playlist_path = PathBuf::from(env_or(
        "RITORNELLO_FILES_MPV_PLAYLIST",
        "/var/lib/ritornello/plugin-files.m3u",
    ));
    let roots_path =
        PathBuf::from(env_or("RITORNELLO_FILES_ROOTS", "/etc/ritornello/media-roots.toml"));
    let creds_dir = PathBuf::from(env_or(
        "RITORNELLO_FILES_CREDENTIALS",
        "/etc/ritornello/media-credentials",
    ));
    let playlists_dir =
        PathBuf::from(env_or("RITORNELLO_FILES_PLAYLISTS", "/var/lib/ritornello/playlists"));
    // Répertoire de travail transitoire, où l'assistant réseau pose son fichier
    // d'authentification le temps d'un appel à `smbclient`.
    //
    // Le **répertoire d'exécution**, et surtout pas celui des identifiants
    // persistés : celui-là vit sous `/etc` et n'est inscriptible qu'en
    // production. Le confondre faisait échouer l'assistant en développement
    // avec un « Permission denied » qui semblait accuser SMB.
    //
    // Même défaut et même variable que le cœur (`RITORNELLO_RUNTIME_DIR`), pour
    // que `docs/development.md` reste vrai d'un binaire à l'autre.
    let runtime_dir = PathBuf::from(env_or("RITORNELLO_RUNTIME_DIR", "/run/ritornello"));
    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));

    let etat = state::load(&state_path);
    let entries: Vec<Entry> = etat.playlist.iter().map(Entry::from).collect();
    // Les pistes absentes sont **conservées** : un partage momentanément
    // injoignable (NAS endormi, montage pas encore fait au boot) effacerait
    // sinon la liste de l'utilisateur.
    //
    // Et surtout : elles ne sont **pas comptées ici**. Ce compte se faisait par
    // un `is_file` sur chaque piste, avant que les deux moitiés ne soient
    // lancées — donc avant que la socket d'admin n'existe. Le 2026-08-17, un
    // montage cifs bloqué dans le noyau y a retenu le démarrage, et la page de
    // gestion a purement disparu de l'IHM : le cœur ne voit un plugin d'admin
    // que s'il a lié sa socket. Rien qui touche un chemin média n'a le droit de
    // s'exécuter avant. La page rend la même information, sous disjoncteur,
    // par le champ `missing` de `get_data`.
    let index = if etat.index < entries.len() { etat.index } else { 0 };

    let roots = Roots::load(&roots_path).unwrap_or_else(|e| {
        tracing::warn!("no usable media-roots.toml ({e}): starting with no root");
        Roots::default()
    });
    let catalog = Arc::new(RwLock::new(Catalog::load("files", "en", &locales_root, FILES_EN)));
    let playlist = Arc::new(AsyncRwLock::new(Playlist { entries, index }));
    let roots = Arc::new(AsyncRwLock::new(roots));
    let (preset_count_tx, preset_count_rx) =
        tokio::sync::watch::channel(playlist.read().await.preset_count());

    // Deux drapeaux partagés entre les deux moitiés : la page modifie la liste,
    // la Source la joue, et rien d'autre ne les relie — les notifications du SDK
    // sont volontairement sans action.
    let liste_changee = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let joue = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Créé ici, sans rien sonder : le disjoncteur n'apprend qu'en servant les
    // requêtes. Sonder au démarrage remettrait un accès disque avant la
    // liaison de la socket, ce qui est précisément le défaut qu'il corrige.
    //
    // Partagé avec la Source : la recherche de pochette fait un `read_dir` sur
    // le même partage que la moitié Admin, et doit tomber sous le même
    // disjoncteur plutôt que d'en inventer un second.
    let sante = Arc::new(ritornello_plugin_files::sante::Sante::new());

    let source = FilesSource {
        playlist: playlist.clone(),
        liste_changee: liste_changee.clone(),
        joue: joue.clone(),
        state_path: state_path.clone(),
        mpv_playlist_path,
        catalog: catalog.clone(),
        locales_root,
        preset_count_rx: Some(preset_count_rx),
        pochette_en_vol: None,
        pochette_par_repertoire: Arc::new(Mutex::new(None)),
        sante: sante.clone(),
    };

    // Sonde au démarrage plutôt qu'à l'usage : la page doit pouvoir griser
    // l'assistant réseau dès son ouverture, comme l'onglet Système grise le
    // redémarrage sur `can_reboot`. La sonde est refaite à chaque tentative de
    // connexion, pour qu'installer le paquet sans redémarrer donne un résultat
    // juste.
    let smb_ok = Arc::new(std::sync::atomic::AtomicBool::new(
        ritornello_plugin_files::smb::available().await,
    ));
    if !smb_ok.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!("smbclient is not available: the network wizard will be offered read-only");
    }

    let admin = admin::FilesAdmin {
        explore: ritornello_plugin_files::explore::Explorateur::new(
            runtime_dir.clone(),
            catalog.clone(),
            smb_ok.clone(),
            sante.clone(),
        ),
        sante,
        mount_error: Arc::new(Mutex::new(None)),
        smb_ok,
        liste_changee,
        joue,
        durees: Arc::new(Mutex::new(admin::DureesProgress::default())),
        durees_task: None,
        roots_path,
        creds_dir,
        internal_playlists: playlists_dir,
        state_path,
        roots,
        playlist,
        catalog,
        scan: Arc::new(Mutex::new(admin::ScanProgress::default())),
        scan_task: None,
        unresolved: Arc::new(Mutex::new(Vec::new())),
        browse: Arc::new(Mutex::new(serde_json::json!({}))),
        preset_count_tx,
    };
    Runtime::from_args()?.source(source)?.admin(admin)?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::IdentityUpdate;

    /// Fabrique une `Metadata` de test pour un couple (cible, niveau).
    ///
    /// `tracing::Metadata::new` demande des `&'static str` : les cibles
    /// testées sont donc des littéraux, ce qui suffit — la règle ne porte que
    /// sur un préfixe connu à l'avance.
    fn trame(cible: &'static str, niveau: tracing::Level) -> tracing::Metadata<'static> {
        tracing::Metadata::new(
            "trame",
            cible,
            niveau,
            None,
            None,
            None,
            tracing::field::FieldSet::new(&[], tracing::callsite::Identifier(&CALLSITE)),
            tracing::metadata::Kind::EVENT,
        )
    }

    /// Un site d'appel factice, exigé par `FieldSet::new`. Il n'est jamais
    /// enregistré ni consulté : seul son identité sert de clé.
    struct Callsite;
    impl tracing::callsite::Callsite for Callsite {
        fn set_interest(&self, _: tracing::subscriber::Interest) {}
        fn metadata(&self) -> &tracing::Metadata<'_> {
            unreachable!("ce site d'appel n'est jamais consulte")
        }
    }
    static CALLSITE: Callsite = Callsite;

    #[test]
    fn le_bavardage_de_lofty_est_ecarte_du_journal_mais_pas_ses_erreurs() {
        // Le symptôme rapporté : « MPEG: Using bitrate to estimate duration »,
        // un WARN par MP3 sans en-tête Xing, qui chasse de vraies erreurs du
        // tampon des « dernières erreurs » du cœur.
        assert!(!trame_a_journaliser(&trame("lofty::mpeg::properties", tracing::Level::WARN)));
        assert!(!trame_a_journaliser(&trame("lofty", tracing::Level::INFO)));
        // Ce que la règle ne doit surtout pas emporter :
        assert!(trame_a_journaliser(&trame("lofty::mpeg", tracing::Level::ERROR)));
        assert!(trame_a_journaliser(&trame("ritornello_plugin_files", tracing::Level::WARN)));
        // Et pas de correspondance par simple sous-chaîne : une cible qui
        // commence par le même mot sans être `lofty` reste journalisée.
        assert!(trame_a_journaliser(&trame("mon_crate::lofty_helper", tracing::Level::WARN)));
    }

    fn source_de_test(playlist: Playlist) -> FilesSource {
        let dir = tempfile::tempdir().unwrap();
        let racine = dir.path().to_path_buf();
        // Le tempdir est volontairement fuité : la Source vit le temps du test,
        // et le laisser tomber effacerait les chemins qu'elle écrit.
        std::mem::forget(dir);
        FilesSource {
            playlist: Arc::new(AsyncRwLock::new(playlist)),
            liste_changee: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            joue: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            state_path: racine.join("plugin-files.json"),
            mpv_playlist_path: racine.join("plugin-files.m3u"),
            catalog: Arc::new(RwLock::new(Catalog::load("files", "en", &racine, FILES_EN))),
            locales_root: racine,
            preset_count_rx: None,
            pochette_en_vol: None,
            pochette_par_repertoire: Arc::new(Mutex::new(None)),
            sante: Arc::new(ritornello_plugin_files::sante::Sante::new()),
        }
    }

    fn liste_de(n: usize) -> Playlist {
        Playlist {
            entries: (1..=n)
                .map(|i| Entry {
                    path: PathBuf::from(format!("/musique/{i:02}.mp3")),
                    title: None,
                    duration_s: None,
                })
                .collect(),
            index: 0,
        }
    }

    #[tokio::test]
    async fn activer_une_liste_vide_ne_lance_rien_et_le_dit() {
        let mut s = source_de_test(Playlist::default());
        let out = s.activate().await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert_eq!(out.preset_count, Some(0));
        assert!(out.status.is_some(), "le statut doit dire pourquoi rien ne joue");
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn activer_reprend_a_la_piste_memorisee() {
        // La reprise après redémarrage : sans `start`, la lecture repartirait à
        // la première piste à chaque démarrage de l'appareil.
        let mut p = liste_de(5);
        p.index = 3;
        let mut s = source_de_test(p);
        let out = s.activate().await;
        match out.action {
            SourceAction::Play { start, finite, .. } => {
                assert_eq!(start, Some(3));
                assert!(finite, "une liste de fichiers a une fin normale");
            }
            autre => panic!("attendu un Play, obtenu {autre:?}"),
        }
        assert_eq!(out.preset, Some(4));
        assert_eq!(out.preset_count, Some(5));
        assert!(out.preset_name.is_some(), "l'ecran ne doit jamais etre muet");
    }

    #[tokio::test]
    async fn une_piste_inexistante_donne_un_message_ephemere_sans_couper_la_lecture() {
        // Même règle que la présélection vide de la radio : rien n'a été lancé,
        // donc la piste précédente joue toujours et doit reparaître à l'écran.
        // Surtout : aucune déclaration d'identité, sans quoi les métadonnées du
        // morceau en cours seraient effacées.
        let mut s = source_de_test(liste_de(3));
        let out = s.select(9).await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert!(out.transient, "le message doit s'effacer de lui-meme");
        assert!(out.identity.is_none(), "declarer un arret serait faux");
        assert_eq!(out.preset_count, Some(3));
    }

    #[tokio::test]
    async fn le_statut_est_redeclare_a_chaque_trame() {
        // PIÈGE : `status` a la convention INVERSE de `preset`. Absent veut dire
        // « pas de statut », et non « garde le précédent » : une Source qui
        // l'omettrait verrait son affichage s'effacer tout seul.
        let mut s = source_de_test(liste_de(3));
        for (nom, out) in [
            ("activate", s.activate().await),
            ("select", s.select(2).await),
            ("next", s.next().await),
            ("prev", s.prev().await),
            ("stop", s.stop().await),
        ] {
            assert!(out.status.is_some(), "statut omis sur {nom} : l'ecran s'effacerait");
        }
    }

    #[tokio::test]
    async fn l_avance_automatique_recale_index_identite_et_nom() {
        // Chemin réel : mpv passe à la piste suivante seul, le cœur relaie
        // `PlayerTrack(n)`, et seule la Source sait ce que « piste n » désigne.
        let mut s = source_de_test(liste_de(5));
        let out = s.player_track(2).await;
        assert_eq!(out.preset, Some(3));
        assert!(out.preset_name.is_some());
        assert_eq!(
            out.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "file", "path": "/musique/03.mp3"
            })))
        );
    }

    #[tokio::test]
    async fn un_index_negatif_est_ecarte_sans_rien_declarer() {
        // mpv dit -1 en fin de liste. Le cœur le transmet tel quel ; la Source
        // l'écarte, et surtout ne déclare rien — l'arrêt viendra de `stop()`.
        let mut s = source_de_test(liste_de(3));
        let out = s.player_track(-1).await;
        assert!(matches!(out.action, SourceAction::Noop));
        assert!(out.identity.is_none());
    }

    #[tokio::test]
    async fn la_fin_de_liste_declare_que_plus_rien_ne_joue() {
        let mut s = source_de_test(liste_de(3));
        let out = s.stop().await;
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn next_et_prev_delegent_a_mpv_sans_recaler_deux_fois() {
        // Recaler ici en plus de `player_track` ferait deux corrections pour un
        // seul changement, et la seconde pourrait contredire la première.
        let mut s = source_de_test(liste_de(3));
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
        assert_eq!(s.prev().await.action, SourceAction::PlayerPrev);
        assert_eq!(s.playlist.read().await.index, 0, "l'index ne doit pas avoir bouge de lui-meme");
    }

    #[tokio::test]
    async fn selectionner_persiste_la_piste() {
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        assert_eq!(state::load(&s.state_path).index, 2);
    }

    #[tokio::test]
    async fn la_moitie_admin_annonce_le_compte_sans_deranger_la_lecture() {
        // Modifier la liste depuis la page doit mettre à jour la grille de la
        // télécommande web tout de suite, sans attendre qu'une piste soit jouée.
        //
        // **Et renuméroter ce qui joue**, ce qui manquait : réordonner la liste
        // change la position de la piste écoutée, et le compteur de l'afficheur
        // restait sur l'ancienne — la page du plugin était juste, le lecteur non.
        //
        // Ce qui reste garanti, et c'est l'essentiel : ni identité, ni statut, ni
        // action. La piste en cours n'est ni interrompue ni redéclarée, seulement
        // renumérotée. Le cœur conserve bien `preset`, `preset_name` et
        // `preset_count` absents — mais **pas** `status`, qu'il remplace, absence
        // comprise (voir le commentaire de `poll_notification`) : c'est son retour
        // anticipé, pour une trame qui ne déclare ni identité ni statut, qui rend
        // cet avis inoffensif.
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut s = source_de_test(liste_de(3));
        s.preset_count_rx = Some(rx);
        s.select(2).await;
        tx.send(7).unwrap();
        let n = s.poll_notification().await.expect("une notification attendue");
        assert_eq!(n.preset_count, Some(7));
        assert_eq!(n.preset, Some(2), "le numero doit suivre la piste ecoutee");
        assert!(n.preset_name.is_some(), "et le nom avec");
        assert!(n.identity.is_none(), "ce qui joue ne doit pas etre redeclare");
        assert!(n.status.is_none(), "ni le statut touche");
        // Les noms voyagent avec le compte : sans eux, la grille garderait les
        // titres d'avant sous les nouveaux numeros — pire qu'aucun titre.
        assert_eq!(
            n.presets.as_deref().map(|p| p.len()),
            Some(3),
            "les preselections nommees doivent accompagner le compte"
        );
    }

    #[tokio::test]
    async fn la_source_enumere_ses_preselections_nommees() {
        // Sans cette surcharge, le corps par defaut de `list_presets` rend une
        // liste vide et les tuiles de la grille n'ont qu'un numero — le defaut
        // signale par le proprietaire. Le catalogue distingue « je n'ai que des
        // numeros » (liste vide) de « voici mes noms », et cette source sait
        // nommer.
        let mut s = source_de_test(liste_de(2));
        let presets = s.list_presets().await;
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].index, 1);
        assert!(!presets[0].name.is_empty(), "chaque tuile doit porter un titre");
    }

    #[tokio::test]
    async fn la_pochette_posee_a_cote_est_annoncee_apres_un_play() {
        // Le cas nominal : un CD rippé pose son `cover.jpg` à côté des pistes.
        // La recherche doit se faire après le `Play`, dans la notification
        // spontanée — pas dans la réponse à `activate()`, qui ne déclare
        // jamais de pochette (voir `serve_source`).
        let dir = tempfile::tempdir().unwrap();
        let piste = dir.path().join("01 - piste.flac");
        std::fs::write(&piste, b"x").unwrap();
        std::fs::write(dir.path().join("cover.jpg"), b"x").unwrap();
        let mut s = source_de_test(Playlist {
            entries: vec![Entry { path: piste, title: None, duration_s: None }],
            index: 0,
        });
        let out = s.activate().await;
        assert!(out.identity.is_some(), "la piste doit etre declaree comme telle");
        let n = s.poll_notification().await.expect("une notification attendue");
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path {
                path: dir.path().join("cover.jpg").to_string_lossy().into_owned()
            })
        );
    }

    #[tokio::test]
    async fn l_avance_automatique_reannonce_la_pochette_sans_re_sonder() {
        // Le cas d'usage phare de toute cette couche, et il etait faux : sur un
        // album ripe, seule la piste que l'utilisateur lance passe par
        // `jouer()`. Les suivantes arrivent par `player_track`, qui repond par
        // `recale()` — une **nouvelle identite**, donc un `cover_source` remis
        // a zero cote coeur (voir `Metadonnees::set_identity`) — sans jamais
        // rearmer la sonde. Resultat : pochette sur la piste 1, repli ♫ sur
        // les pistes 2..N.
        let dir = tempfile::tempdir().unwrap();
        let pochette = dir.path().join("cover.jpg");
        std::fs::write(&pochette, b"x").unwrap();
        let entrees: Vec<Entry> = (1..=3)
            .map(|i| {
                let p = dir.path().join(format!("{i:02} - piste.flac"));
                std::fs::write(&p, b"x").unwrap();
                Entry { path: p, title: None, duration_s: None }
            })
            .collect();
        let mut s = source_de_test(Playlist { entries: entrees, index: 0 });
        let attendue = Some(ritornello_proto::CoverRef::Path {
            path: pochette.to_string_lossy().into_owned(),
        });

        s.activate().await;
        assert_eq!(s.poll_notification().await.unwrap().cover, attendue, "piste 1");

        // mpv avance de lui-meme. La pochette doit repartir avec la nouvelle
        // identite.
        let out = s.player_track(1).await;
        assert!(out.identity.is_some(), "une nouvelle identite est bien declaree");
        assert_eq!(s.poll_notification().await.unwrap().cover, attendue, "piste 2");

        // Et **sans repayer le `readdir`** : le repertoire n'a pas change. La
        // preuve se fait en retirant l'image du disque — une sonde reelle ne
        // trouverait plus rien, la valeur memorisee est pourtant reannoncee.
        // C'est ce qui evite un aller-retour SMB a chaque changement de piste.
        std::fs::remove_file(&pochette).unwrap();
        s.player_track(2).await;
        assert_eq!(
            s.poll_notification().await.unwrap().cover,
            attendue,
            "piste 3 : la valeur doit venir de la memoire, pas d'une nouvelle sonde"
        );
    }

    #[tokio::test]
    async fn changer_de_repertoire_resonde() {
        // Le pendant du test ci-dessus : la memorisation est par repertoire,
        // donc passer a un album voisin doit bien sonder de nouveau — sans
        // quoi le second album afficherait la pochette du premier.
        let dir = tempfile::tempdir().unwrap();
        let un = dir.path().join("album-un");
        let deux = dir.path().join("album-deux");
        std::fs::create_dir_all(&un).unwrap();
        std::fs::create_dir_all(&deux).unwrap();
        std::fs::write(un.join("cover.jpg"), b"x").unwrap();
        std::fs::write(deux.join("folder.png"), b"x").unwrap();
        let entrees = vec![
            Entry { path: un.join("01.flac"), title: None, duration_s: None },
            Entry { path: deux.join("01.flac"), title: None, duration_s: None },
        ];
        let mut s = source_de_test(Playlist { entries: entrees, index: 0 });
        s.activate().await;
        assert_eq!(
            s.poll_notification().await.unwrap().cover,
            Some(ritornello_proto::CoverRef::Path {
                path: un.join("cover.jpg").to_string_lossy().into_owned()
            })
        );
        s.player_track(1).await;
        assert_eq!(
            s.poll_notification().await.unwrap().cover,
            Some(ritornello_proto::CoverRef::Path {
                path: deux.join("folder.png").to_string_lossy().into_owned()
            }),
            "un repertoire different doit etre sonde"
        );
    }

    #[tokio::test]
    async fn l_absence_de_pochette_ne_bloque_pas_les_autres_notifications() {
        // Défendu par la revue : `poll_notification` ne doit jamais rendre
        // `None` (terminal pour le SDK) ni une notification vide quand il n'y
        // a rien à côté du fichier. La preuve : le mécanisme du compte de
        // présélections, sans rapport, continue de fonctionner juste après.
        let dir = tempfile::tempdir().unwrap();
        let piste = dir.path().join("01 - piste.flac");
        std::fs::write(&piste, b"x").unwrap(); // pas d'image a cote
        let (tx, rx) = tokio::sync::watch::channel(0u8);
        let mut s = source_de_test(Playlist {
            entries: vec![Entry { path: piste, title: None, duration_s: None }],
            index: 0,
        });
        s.preset_count_rx = Some(rx);
        s.activate().await;
        tx.send(3).unwrap();
        let n = s.poll_notification().await.expect("une notification attendue, pas None");
        assert_eq!(n.preset_count, Some(3));
        assert!(n.cover.is_none(), "aucune pochette a cote, rien a annoncer");
    }

    #[tokio::test]
    async fn annuler_puis_repoller_ne_relance_pas_une_seconde_sonde() {
        // Défendu par la revue : un appel direct à `sante.borne(...).await`
        // depuis `poll_notification` se ferait annuler par le `select!` du SDK
        // dès qu'une requête du cœur arrive — un événement courant, pas un cas
        // limite — perdant la comptabilité de `sante` et relançant une sonde
        // de plus sur le même partage à chaque tour. La correction : la sonde
        // vit sur une tâche indépendante (`jouer()` la lance), et
        // `poll_notification` ne fait qu'attendre son résultat sur un
        // `oneshot::Receiver`, cancel-safe par construction.
        //
        // Compter les sondes réellement lancées demanderait d'instrumenter
        // `pochette::cherche` ou de forcer un vrai délai de `sante` — ce qui
        // retombe sur une hypothèse de timing que ce crate vient justement
        // d'expulser de ses tests (voir l'historique). À la place, ce test
        // vérifie directement la propriété qui rend la seconde sonde
        // impossible : le récepteur unique posé ici survit intact à
        // l'annulation d'un premier tour de `poll_notification`, et le second
        // tour lit sa réponse sur ce même canal — sans qu'aucun code de
        // `poll_notification` n'ait besoin d'en ouvrir un autre (il n'y a tout
        // simplement aucun `tokio::spawn` dans cette méthode).
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut s = source_de_test(Playlist::default());
        s.pochette_en_vol = Some(rx);

        // Premier tour : rien n'a encore été envoyé, `poll_notification` doit
        // rester en attente. `yield_now()` borne l'observation à un seul
        // passage du planificateur — une propriété déterministe du runtime,
        // pas une horloge murale, donc aucune place pour un flake ici.
        tokio::select! {
            _ = s.poll_notification() => panic!("ne doit pas resoudre avant que quelque chose soit envoye"),
            _ = tokio::task::yield_now() => {}
        }

        // Le futur précédent a été abandonné (l'équivalent exact de
        // l'annulation par le `select!` du SDK). Le champ doit avoir survécu
        // intact, toujours branché sur cet unique récepteur : si
        // `poll_notification` en avait ouvert un second, ce `send` — le seul
        // émetteur qui existe dans ce test — n'aurait personne côté second
        // canal fictif à convaincre, et l'assertion suivante échouerait en
        // rendant `None` plutôt que la pochette.
        tx.send(Some(ritornello_proto::CoverRef::Path { path: "/nas/Album/cover.jpg".into() }))
            .unwrap();
        let n = s.poll_notification().await.expect("une notification attendue, pas None");
        assert_eq!(
            n.cover,
            Some(ritornello_proto::CoverRef::Path { path: "/nas/Album/cover.jpg".into() })
        );
    }

    #[tokio::test]
    async fn le_statut_suit_le_catalogue_apres_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(dir.path().join("files/fr.toml"), "status_files = \"FICHIERS\"\n").unwrap();
        let mut s = source_de_test(liste_de(2));
        s.locales_root = dir.path().to_path_buf();
        s.set_locale("fr".into()).await;
        assert_eq!(s.activate().await.status.as_deref(), Some("FICHIERS"));
    }

    #[tokio::test]
    async fn un_arret_sur_liste_vide_dit_que_la_liste_est_vide() {
        // Défaut signalé, et il était mesquin : `jouer()` affichait bien
        // « AUCUNE LISTE », mais sans piste mpv reste inactif, le cœur envoyait
        // donc `stop()` aussitôt — et cette trame écrasait le message par un
        // statut générique. L'utilisateur ne pouvait pas apprendre que sa liste
        // était vide.
        let mut s = source_de_test(Playlist::default());
        assert_eq!(s.activate().await.status.as_deref(), Some("NO PLAYLIST"));
        assert_eq!(s.stop().await.status.as_deref(), Some("NO PLAYLIST"));
    }

    #[tokio::test]
    async fn un_arret_sur_une_liste_pleine_reste_un_arret_ordinaire() {
        // Garde-fou : « aucune liste » doit rester réservé au cas où il n'y a
        // vraiment rien à jouer.
        let mut s = source_de_test(liste_de(3));
        s.activate().await;
        assert_eq!(s.stop().await.status.as_deref(), Some("FILES"));
    }

    #[tokio::test]
    async fn un_arret_annonce_la_piste_armee_et_non_un_statut_nu() {
        // Défaut signalé : l'afficheur se retrouvait « perdu » — le statut deux
        // fois, sans numéro ni nom — après un arrêt. Il doit dire l'état réel :
        // rien ne joue, et voilà ce qui repartira.
        let mut s = source_de_test(liste_de(3));
        s.select(2).await;
        let issue = s.stop().await;
        assert_eq!(issue.preset, Some(2), "la piste armee reste designee");
        assert!(issue.preset_name.is_some(), "et nommee");
        assert_eq!(issue.preset_count, Some(3));
        // Mais rien ne joue : c'est ce que `plays_nothing` déclare, et c'est ce
        // qui fait disparaître le bloc « en cours de lecture » de l'afficheur.
        assert!(issue.identity.is_some(), "l'arret doit etre declare, pas tu");
    }

    #[tokio::test]
    async fn un_arret_sur_liste_vide_ne_designe_aucune_piste() {
        // Rien à armer : annoncer un numéro désignerait une piste qui n'existe
        // pas.
        let mut s = source_de_test(Playlist::default());
        let issue = s.stop().await;
        assert_eq!(issue.status.as_deref(), Some("NO PLAYLIST"));
        assert!(issue.preset.is_none());
        assert_eq!(issue.preset_count, Some(0));
    }

    #[tokio::test]
    async fn un_playlist_pos_negatif_ne_deplace_pas_l_index() {
        // mpv annonce `-1` en fin de liste **et transitoirement à chaque
        // rechargement**, donc à chaque changement de piste : c'est mesuré. En
        // tirer une conclusion — « la liste est terminée, repartons du début » —
        // faisait retomber toute reprise sur la piste 1.
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        s.player_track(-1).await;
        assert_eq!(s.activate().await.preset, Some(3), "le -1 ne doit rien conclure");
    }

    #[tokio::test]
    async fn une_liste_est_declaree_comme_telle_au_coeur() {
        // Le défaut central : sans `playlist`, le cœur chargeait le m3u par
        // `loadfile`, que mpv ne déplie qu'après coup — l'index de départ
        // arrivait hors bornes et toute sélection rejouait la première piste.
        let mut s = source_de_test(liste_de(3));
        match s.select(2).await.action {
            SourceAction::Play { playlist, start, finite, .. } => {
                assert!(playlist, "un m3u doit etre charge comme une liste");
                assert_eq!(start, Some(1), "piste 2 = index 1");
                assert!(finite, "une liste de fichiers a une fin normale");
            }
            autre => panic!("attendu un Play, recu {autre:?}"),
        }
    }

    #[tokio::test]
    async fn reprendre_apres_un_arret_rend_la_piste_ecoutee() {
        // La touche Lecture après un Stop redemande `activate()`. Elle doit
        // rendre la piste qu'on écoutait — l'index vit dans le plugin et aucun
        // arrêt ne le déplace — et non repartir de la première.
        let mut s = source_de_test(liste_de(4));
        s.select(3).await;
        s.stop().await;
        assert_eq!(s.activate().await.preset, Some(3), "la piste ecoutee, pas la premiere");
    }

    #[tokio::test]
    async fn suivant_delegue_a_mpv_quand_la_liste_na_pas_bouge() {
        // Le cas ordinaire : mpv tient la même liste que nous, il sait avancer
        // seul. Recharger ici couperait le son pour rien.
        let mut s = source_de_test(liste_de(3));
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
    }

    #[tokio::test]
    async fn suivant_rend_la_liste_a_jour_quand_la_page_l_a_modifiee() {
        // Défaut de conception signalé : mpv joue une **copie** de la liste,
        // écrite au dernier `Play`. Une piste ajoutée depuis lui était hors
        // d'atteinte, une piste retirée revenait. La moitié Admin ne pouvant rien
        // lui dire, on saisit le premier ordre explicite pour lui rendre la liste
        // neuve — un moment où l'utilisateur attend de toute façon un changement.
        let mut s = source_de_test(liste_de(4));
        s.select(2).await;
        s.liste_changee.store(true, std::sync::atomic::Ordering::Relaxed);
        let issue = s.next().await;
        assert!(matches!(issue.action, SourceAction::Play { .. }), "{:?}", issue.action);
        assert_eq!(issue.preset, Some(3), "la piste qui suit, dans la nouvelle liste");
        // L'écart est refermé : l'ordre suivant redélègue à mpv.
        assert_eq!(s.next().await.action, SourceAction::PlayerNext);
    }

    #[tokio::test]
    async fn precedent_boucle_plutot_que_de_bloquer_apres_une_modification() {
        // Sans bouclage, une modification qui laisse l'auditeur sur la première
        // piste rendrait « précédent » inopérant, alors que mpv boucle d'un bout
        // à l'autre de sa propre liste.
        let mut s = source_de_test(liste_de(3));
        s.liste_changee.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(s.prev().await.preset, Some(3), "on revient a la derniere piste");
    }

    #[tokio::test]
    async fn le_changement_de_piste_automatique_rend_la_liste_a_jour() {
        // Le défaut signalé à l'usage : ne resynchroniser qu'au prochain ordre
        // explicite ne suffisait pas. Si l'on modifie la liste et qu'on laisse
        // simplement la piste s'achever, mpv enchaînait dans l'ancienne — donc
        // « les modifications de playlist, rien ».
        //
        // Le changement automatique est au contraire le meilleur moment : mpv
        // démarre un fichier de toute façon, rien n'est interrompu.
        let mut s = source_de_test(liste_de(4));
        s.select(2).await;
        s.liste_changee.store(true, std::sync::atomic::Ordering::Relaxed);
        let issue = s.player_track(2).await;
        assert!(matches!(issue.action, SourceAction::Play { .. }), "{:?}", issue.action);
        assert_eq!(issue.preset, Some(3), "la piste qui suit, dans la liste a jour");
    }

    #[tokio::test]
    async fn un_changement_de_piste_sans_modification_ne_recharge_rien() {
        // Le cas ordinaire, et de loin le plus fréquent : recharger ici
        // couperait le son à chaque changement de piste.
        let mut s = source_de_test(liste_de(4));
        s.select(1).await;
        let issue = s.player_track(1).await;
        assert!(matches!(issue.action, SourceAction::Noop), "{:?}", issue.action);
        assert_eq!(issue.preset, Some(2), "on ne fait que redire ou mpv en est");
    }

    #[tokio::test]
    async fn la_fin_de_liste_ne_relance_pas_une_liste_modifiee() {
        // À `-1` la liste est terminée. Recharger là relancerait la lecture au
        // lieu de la laisser finir — une liste qui boucle sans qu'on l'ait
        // demandé.
        let mut s = source_de_test(liste_de(3));
        s.liste_changee.store(true, std::sync::atomic::Ordering::Relaxed);
        let issue = s.player_track(-1).await;
        assert!(matches!(issue.action, SourceAction::Noop), "{:?}", issue.action);
    }

    #[tokio::test]
    async fn suivant_sur_une_liste_videe_ne_joue_rien() {
        // Vider pendant la lecture : l'arrêt est demandé par la page, mais si un
        // ordre arrive quand même, il ne doit pas chercher une piste inexistante.
        let mut s = source_de_test(Playlist::default());
        s.liste_changee.store(true, std::sync::atomic::Ordering::Relaxed);
        let issue = s.next().await;
        assert_eq!(issue.status.as_deref(), Some("NO PLAYLIST"));
        assert!(matches!(issue.action, SourceAction::Noop));
    }

    #[test]
    fn en_embarque_files_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(FILES_EN).unwrap().is_empty());
    }
}
