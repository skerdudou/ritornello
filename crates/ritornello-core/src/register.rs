//! Rassemblement des annonces des greffons.
//!
//! Le cœur lie un socket avant tout lancement, puis attend une annonce par
//! greffon lancé. Comme le greffon lie ses propres sockets **avant** de
//! s'annoncer, la ligne reçue est une barrière de disponibilité : le cœur
//! peut se connecter derrière sans retenter. C'est ce qui remplace les deux
//! attentes devinées d'avant — la fenêtre de 2 s de la page d'admin et les
//! 10 s de reprises de connexion.

use futures::{Stream, StreamExt};
use ritornello_proto::{Announcement, PluginKind};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

/// Ce que le rassemblement a appris.
///
/// Les deux listes de muets sont **séparées** parce que le cœur en fait deux
/// choses différentes : un figé garde une chance de parler — le socket
/// d'enregistrement reste ouvert pour toute la vie du processus — tandis qu'un
/// mort n'en a plus aucune. Les confondre, c'était rapporter la même ligne de
/// statut pour deux pannes qui ne se réparent pas de la même façon.
#[derive(Debug, Default)]
pub struct Gathered {
    /// Annoncés, par nom.
    pub announcements: HashMap<String, Announcement>,
    /// Lancés, jamais annoncés, et dont la mort n'a **pas** été observée :
    /// vivants et muets à l'échéance. Nommés, pour que le journal désigne un
    /// coupable au lieu de laisser déduire.
    pub figes: Vec<String>,
    /// Lancés et morts sans laisser d'annonce exploitable : soit morts avant
    /// de parler, soit morts pendant le rassemblement après avoir parlé (leur
    /// annonce est alors retirée, voir la branche des décès).
    pub morts: Vec<String>,
}

/// Temps laissé à une connexion pour écrire sa ligne d'annonce.
///
/// Une annonce est écrite dans la foulée du `connect` par le SDK : quelques
/// secondes couvrent un appareil chargé avec une marge large, et ce qui n'a
/// rien dit passé ce délai n'est pas un greffon lent mais un greffon fautif.
const DELAI_LECTURE: Duration = Duration::from_secs(5);

/// Lit **une** ligne d'annonce sur une connexion acceptée, la déchiffre, et la
/// pousse dans le canal des annonces.
///
/// Une tâche par connexion : une connexion muette ne doit retarder ni le
/// rendez-vous ni les annonces tardives.
///
/// La lecture est **bornée** dans le temps. Sans cela, chaque connexion muette
/// immobilisait une tâche et un descripteur pour la vie du processus : un
/// greffon avec un bug de reconnexion, qui frappe le socket une fois par
/// seconde sans jamais écrire, finissait par épuiser les descripteurs — et le
/// cœur, à qui l'`accept` échoue alors en permanence, ne pouvait plus câbler
/// **aucune** annonce sur un appareil qu'on ne redémarre jamais. Le
/// rassemblement avait le même lecteur mais son échéance le bornait ; la boucle
/// permanente, elle, n'en a aucune.
async fn lit_annonce(
    stream: tokio::net::UnixStream,
    tx: tokio::sync::mpsc::Sender<Announcement>,
    delai: Duration,
) {
    let mut lignes = BufReader::new(stream).lines();
    match tokio::time::timeout(delai, lignes.next_line()).await {
        Ok(Ok(Some(l))) => match serde_json::from_str::<Announcement>(&l) {
            Ok(a) => {
                let _ = tx.send(a).await;
            }
            Err(e) => tracing::warn!("unreadable announcement ignored ({e}): {l}"),
        },
        Ok(Ok(None)) => {
            tracing::warn!("a plugin connected to the register socket and said nothing")
        }
        Ok(Err(e)) => tracing::warn!("reading an announcement failed: {e}"),
        // La connexion est lâchée en sortant : la tâche et le descripteur sont
        // rendus, c'est tout l'objet du délai.
        Err(_) => tracing::warn!(
            "a plugin held the register socket open for {}s without announcing, dropping it",
            delai.as_secs()
        ),
    }
}

/// Attend une annonce par greffon lancé.
///
/// Rend la main dès que chaque attendu est soit annoncé, soit mort — donc en
/// pratique bien avant `echeance`. Un délai ne se paie plus qu'à l'échec.
///
/// `annonces_tx` / `annonces_rx` sont le **canal unique des deux étages** :
/// celui que `accept_forever` alimentera ensuite, et que la boucle principale
/// consomme. Le rassemblement n'a pas son propre canal, et c'est ce qui rend
/// une annonce inperdable : quand une annonce et l'échéance sont prêtes au même
/// instant, `tokio::select!` tire au hasard, et le tirage ne décide plus que du
/// chemin. Ce que `gather` ne consomme pas — l'annonce prête à l'échéance, et
/// celle des connexions déjà acceptées dont la tâche de lecture n'a pas encore
/// abouti — **reste en file** pour le câblage à chaud. Avec un canal propre au
/// rassemblement, détruit à son retour, elle partait avec le récepteur : le
/// greffon, qui n'annonce qu'une fois, se croyait enregistré et attendait le
/// prochain redémarrage du service, sans une ligne de journal.
pub async fn gather<S>(
    listener: &UnixListener,
    attendus: &[String],
    deces: S,
    echeance: Duration,
    annonces_tx: &tokio::sync::mpsc::Sender<Announcement>,
    annonces_rx: &mut tokio::sync::mpsc::Receiver<Announcement>,
) -> Gathered
where
    S: Stream<Item = String> + Unpin,
{
    // `restants` = ceux qu'on attend encore. Une mort précoce en sort (cesser
    // d'attendre) mais reste un muet : les deux listes de muets sont donc
    // calculées à la fin depuis `attendus`, et non reprises de `restants` —
    // sinon un greffon mort avant de s'annoncer disparaissait du rapport,
    // exactement le diagnostic que ce rassemblement existe pour nommer.
    let mut restants: Vec<String> = attendus.to_vec();
    let mut announcements: HashMap<String, Announcement> = HashMap::new();
    // Les morts **observées**. C'est ce qui sépare un muet vivant d'un muet
    // mort : sans cette trace, l'échéance ne pourrait que déduire, et un
    // greffon simplement lent serait rapporté comme un greffon perdu.
    let mut deces_vus: Vec<String> = Vec::new();
    let mut deces = deces.fuse();
    let fin = tokio::time::sleep(echeance);
    tokio::pin!(fin);

    // **Une tâche de lecture par connexion**, et non une lecture en ligne dans
    // la branche `accept` : un greffon qui se connecte puis n'écrit rien ne
    // doit pas retarder l'annonce des autres. Un blocage de tête sur le
    // rendez-vous serait le défaut même que le protocole refuse ailleurs.
    //
    // C'est la même tâche que celle d'`accept_forever`, vers le même canal :
    // une connexion acceptée ici mais lue après le retour de `gather` n'est pas
    // perdue pour autant, son annonce attend simplement dans la file.
    //
    // L'émetteur d'origine vit chez l'appelant, au-delà de cette fonction :
    // `recv()` ne rend donc jamais `None`, et sa branche du `select!` ne se
    // désarme pas.
    while !restants.is_empty() {
        tokio::select! {
            accepte = listener.accept() => {
                match accepte {
                    Ok((stream, _)) => {
                        tokio::spawn(lit_annonce(stream, annonces_tx.clone(), DELAI_LECTURE));
                    }
                    Err(e) => tracing::warn!("register socket accept failed: {e}"),
                }
            }
            Some(annonce) = annonces_rx.recv() => {
                // Le nom fait autorité côté manifeste : une annonce qui en
                // porte un autre vient d'un binaire mal lancé, ou d'un
                // greffon qui invente son identité. Elle est nommée puis
                // écartée, jamais câblée.
                if !restants.contains(&annonce.name) {
                    if announcements.contains_key(&annonce.name) {
                        tracing::warn!("duplicate announcement for {}, ignored", annonce.name);
                    } else {
                        tracing::warn!("announcement from unknown plugin {}, ignored", annonce.name);
                    }
                    continue;
                }
                restants.retain(|n| n != &annonce.name);
                tracing::info!("{} announced {:?} (admin: {})", annonce.name, annonce.kinds, annonce.admin);
                announcements.insert(annonce.name.clone(), annonce);
            }
            Some(mort) = deces.next() => {
                // La mort est **observée** ici, et nulle part ailleurs : c'est
                // cette liste qui permettra plus bas de nommer un muet vivant
                // (figé) plutôt que de le confondre avec un muet mort.
                if !deces_vus.contains(&mort) {
                    deces_vus.push(mort.clone());
                }
                // Le processus est parti avant de s'annoncer : cesser de
                // l'attendre. C'est ce qui rend un plantage au démarrage plus
                // rapide à diagnostiquer qu'avant, où il consommait les 10 s
                // de reprises à vide.
                if restants.contains(&mort) {
                    tracing::warn!("plugin {mort} exited before announcing");
                    restants.retain(|n| n != &mort);
                } else if announcements.remove(&mort).is_some() {
                    // Mort **après** s'être annoncé, pendant qu'on attendait
                    // encore quelqu'un d'autre. Sa future a quitté
                    // `plugin_waits` en étant consommée ici : la boucle de
                    // sélection de `main` ne la reverra jamais, ni son code de
                    // sortie, ni son `mark_plugin_disconnected`. Sans ce
                    // retrait, il serait câblé puis affiché « connecté » à
                    // demeure — la perte silencieuse même que ce rendez-vous
                    // existe pour supprimer, et d'autant plus pour les genres
                    // `input` et `metadata` dont le statut est posé à vrai
                    // sans attendre la tâche.
                    //
                    // Le retirer des annonces suffit : les muets étant calculés
                    // à la fin par différence, il retombe tout seul dans
                    // `morts` — sa mort vient d'être observée — et `main` lui
                    // pose un `connected: false` comme aux autres.
                    //
                    // Journal distinct de celui d'au-dessus : « mort avant de
                    // s'annoncer » et « mort pendant le rassemblement » ne
                    // sont pas la même panne.
                    tracing::warn!("plugin {mort} exited during registration");
                }
            }
            () = &mut fin => {
                tracing::warn!("register deadline reached, still waiting for: {}", restants.join(", "));
                break;
            }
        }
    }

    // Dans l'ordre de `attendus`, donc dans l'ordre du manifeste : le journal
    // désigne les coupables dans l'ordre où l'opérateur les a déclarés
    // (`partition` conserve l'ordre de la source).
    //
    // La partition se fait sur la mort **observée**, jamais sur l'échéance : un
    // greffon dont personne n'a vu le processus sortir est présumé vivant, donc
    // figé, donc encore câblable à chaud.
    let (morts, figes): (Vec<String>, Vec<String>) = attendus
        .iter()
        .filter(|nom| !announcements.contains_key(*nom))
        .cloned()
        .partition(|nom| deces_vus.contains(nom));

    Gathered { announcements, figes, morts }
}

/// Continue d'accepter sur le socket d'enregistrement **pour toute la vie du
/// processus**, et pousse chaque annonce lisible dans `tx`.
///
/// C'est ce qui retire à l'échéance de `gather` son pouvoir de condamner : elle
/// ne sert plus qu'à ne pas bloquer le démarrage et à nommer un greffon figé.
/// Le cœur possède ce socket, il peut donc écouter aussi longtemps qu'il vit —
/// un greffon qui s'annonce à t+12 s, ou qu'on relance à la main un mois plus
/// tard, est câblé à chaud au lieu d'être perdu jusqu'au prochain redémarrage
/// du service.
///
/// Ne rend la main que si `tx` est fermé, c'est-à-dire si la boucle principale
/// est morte : plus personne pour câbler quoi que ce soit.
pub async fn accept_forever(listener: UnixListener, tx: tokio::sync::mpsc::Sender<Announcement>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                // **Une tâche de lecture par connexion**, comme dans `gather` :
                // une connexion muette ne doit pas plus bloquer les annonces
                // tardives qu'elle ne bloquait les initiales. Le blocage de
                // tête a déjà été corrigé une fois sur ce socket, il n'est pas
                // réintroduit ici.
                tokio::spawn(lit_annonce(stream, tx.clone(), DELAI_LECTURE));
            }
            Err(e) => {
                tracing::warn!("register socket accept failed: {e}");
                // Cette boucle-ci n'est bornée par aucune échéance,
                // contrairement à celle de `gather` : une erreur durable — plus
                // un descripteur libre — la ferait tourner à vide et à pleine
                // charge sur un appareil qui n'a qu'un petit processeur. Un
                // souffle avant de réessayer.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Les greffons `metadata`, **dans l'ordre du manifeste**.
///
/// L'ordre du fichier est la priorité d'arbitrage : entre deux greffons qui
/// répondent pour le même morceau, le premier déclaré gagne. Avant, la liste
/// était bâtie depuis le manifeste avant tout lancement, donc l'ordre était
/// acquis par construction ; il est maintenant reconstruit ici, et un tri par
/// ordre d'arrivée des annonces rendrait l'affichage non reproductible d'un
/// démarrage à l'autre. Ne jamais trier cette liste autrement.
pub fn metadata_order(manifeste: &[String], g: &Gathered) -> Vec<String> {
    manifeste
        .iter()
        .filter(|nom| {
            g.announcements
                .get(*nom)
                .is_some_and(|a| a.kinds.contains(&PluginKind::Metadata))
        })
        .cloned()
        .collect()
}

/// Reste-t-il, à l'échéance, un processus de greffon **vivant** — donc
/// susceptible de s'annoncer plus tard ?
///
/// C'est la seule condition qui empêche encore le cœur de démarrer. Un greffon
/// lent n'est plus une erreur : le socket d'enregistrement reste ouvert, une
/// annonce à t+30 s est câblée à chaud, et la page de statut doit précisément
/// être **là** pour montrer ce greffon figé. Refuser de démarrer à t+10 s la
/// supprimait au moment où on voulait la consulter, et systemd rebouclait sans
/// rien réparer.
///
/// Mais si plus rien ne tourne — `plugins.toml` vide, exécutables introuvables,
/// ou tous morts avant l'échéance — personne ne s'annoncera jamais. C'est une
/// erreur de configuration, pas une lenteur, et démarrer silencieusement un
/// appareil qui ne jouera jamais rien n'aide personne.
///
/// Déduit de `lances` et de `morts` plutôt que de `announcements` et `figes` :
/// on ne suppose pas que les trois collections partitionnent `lances`, on
/// n'exclut que ce dont la mort a été **observée**.
pub fn un_greffon_vivant(lances: &[String], g: &Gathered) -> bool {
    lances.iter().any(|nom| !g.morts.contains(nom))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::PluginKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    /// Écrit une annonce sur le socket d'enregistrement, comme le ferait un
    /// greffon, puis ferme.
    async fn annonce(register: &std::path::Path, ligne: &str) {
        let mut s = UnixStream::connect(register).await.unwrap();
        s.write_all(format!("{ligne}\n").as_bytes()).await.unwrap();
        s.shutdown().await.unwrap();
    }

    fn aucun_mort() -> impl futures::Stream<Item = String> + Unpin {
        futures::stream::pending()
    }

    /// Le canal unique des deux étages, monté comme dans `main` : `gather`
    /// l'emprunte, `accept_forever` en garde l'émetteur, et la boucle
    /// principale consomme ce que le rassemblement a laissé.
    fn canal() -> (
        tokio::sync::mpsc::Sender<Announcement>,
        tokio::sync::mpsc::Receiver<Announcement>,
    ) {
        tokio::sync::mpsc::channel(16)
    }

    #[tokio::test]
    async fn rassemble_toutes_les_annonces_et_rend_la_main_aussitot() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"],"admin":true}"#).await;
            annonce(&r, r#"{"name":"console","kinds":["display"]}"#).await;
        });

        let debut = std::time::Instant::now();
        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string(), "console".to_string()],
            aucun_mort(),
            Duration::from_secs(10),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.announcements.len(), 2);
        assert!(g.figes.is_empty());
        assert!(g.morts.is_empty());
        assert!(g.announcements["radio"].admin);
        assert_eq!(g.announcements["console"].kinds, vec![PluginKind::Display]);
        assert!(
            debut.elapsed() < Duration::from_secs(2),
            "la boucle doit rendre la main des que tout le monde est la, pas a l'echeance"
        );
    }

    #[tokio::test]
    async fn un_greffon_muet_est_nomme_a_lecheance() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string(), "muet".to_string()],
            aucun_mort(),
            Duration::from_millis(300),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.announcements.len(), 1);
        // Vivant, muet : figé, et non mort — personne n'a vu son processus
        // sortir.
        assert_eq!(g.figes, vec!["muet".to_string()]);
        assert!(g.morts.is_empty());
    }

    #[tokio::test]
    async fn une_mort_precoce_ecourte_lattente() {
        // Aujourd'hui un greffon qui plante fait tourner 10 s de reprises a
        // vide. Ici, `child.wait()` doit trancher tout de suite.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let debut = std::time::Instant::now();
        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string(), "plante".to_string()],
            Box::pin(futures::stream::iter(vec!["plante".to_string()])),
            Duration::from_secs(30),
            &tx,
            &mut rx,
        )
        .await;

        // Un mort, pas un figé : sa sortie a été observée.
        assert_eq!(g.morts, vec!["plante".to_string()]);
        assert!(g.figes.is_empty());
        assert!(
            debut.elapsed() < Duration::from_secs(2),
            "la mort du processus doit ecourter l'attente, pas la subir"
        );
    }

    #[tokio::test]
    async fn a_lecheance_un_vivant_muet_est_fige_et_un_mort_ne_lest_pas() {
        // Les deux muets dans le même rassemblement : c'est la seule façon de
        // vérifier que la partition les sépare, et non qu'une des deux listes
        // ramasse tout. `plante` meurt sous nos yeux, `dort` ne dit rien mais
        // personne n'a vu son processus sortir — il pourra donc encore
        // s'annoncer, et le cœur le câblera à chaud.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["plante".to_string(), "dort".to_string()],
            Box::pin(futures::stream::iter(vec!["plante".to_string()])),
            Duration::from_millis(300),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.morts, vec!["plante".to_string()]);
        assert_eq!(g.figes, vec!["dort".to_string()]);
    }

    #[tokio::test]
    async fn un_nom_inconnu_est_ignore_sans_bloquer_les_autres() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"intrus","kinds":["source"]}"#).await;
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_secs(5),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(g.announcements.len(), 1);
        assert!(g.announcements.contains_key("radio"));
        assert!(!g.announcements.contains_key("intrus"));
    }

    #[tokio::test]
    async fn une_mort_apres_annonce_retire_le_greffon_du_rassemblement() {
        // Fenetre reelle : un greffon rapide s'annonce puis meurt pendant que
        // le coeur attend encore un muet. Sa future a quitte `plugin_waits` en
        // etant consommee par le rassemblement, donc la boucle de selection de
        // `main` ne la reverra jamais — ni son code de sortie, ni son
        // `mark_plugin_disconnected`. S'il restait dans les annonces il serait
        // cable, puis affiche « connecte » a demeure.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        // La mort n'arrive qu'APRES l'annonce : c'est tout l'objet du test. Un
        // flux immediat emprunterait l'autre branche (« mort avant de
        // s'annoncer »), deja couverte par
        // `une_mort_precoce_ecourte_lattente`.
        let morts = Box::pin(futures::stream::once(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            "radio".to_string()
        }));

        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string(), "muet".to_string()],
            morts,
            Duration::from_millis(800),
            &tx,
            &mut rx,
        )
        .await;

        assert!(
            !g.announcements.contains_key("radio"),
            "un greffon mort pendant le rassemblement ne doit pas rester cablable"
        );
        assert_eq!(g.morts, vec!["radio".to_string()]);
        assert_eq!(g.figes, vec!["muet".to_string()]);
    }

    #[tokio::test]
    async fn une_annonce_illisible_ne_compte_pas() {
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let r = register.clone();
        tokio::spawn(async move {
            annonce(&r, "ceci n'est pas du json").await;
        });

        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_millis(300),
            &tx,
            &mut rx,
        )
        .await;

        assert!(g.announcements.is_empty());
        // Le processus est toujours là : illisible ne veut pas dire mort.
        assert_eq!(g.figes, vec!["radio".to_string()]);
        assert!(g.morts.is_empty());
    }

    #[tokio::test]
    async fn une_connexion_muette_ne_retarde_pas_les_autres() {
        // Blocage de tete : si la ligne etait lue dans la branche `accept`, un
        // greffon connecte et silencieux gelerait l'annonce de TOUS les autres
        // jusqu'a l'echeance. C'est le defaut que la tache de lecture par
        // connexion existe pour empecher.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        let r = register.clone();
        tokio::spawn(async move {
            // Se connecte, se tait, et garde la connexion ouverte.
            let muet = UnixStream::connect(&r).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(muet);
        });
        let r2 = register.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            annonce(&r2, r#"{"name":"radio","kinds":["source"]}"#).await;
        });

        let debut = std::time::Instant::now();
        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_secs(30),
            &tx,
            &mut rx,
        )
        .await;

        assert_eq!(
            g.announcements.len(),
            1,
            "l'annonce doit passer malgre la connexion muette"
        );
        assert!(
            debut.elapsed() < Duration::from_secs(5),
            "une connexion muette ne doit pas retarder le rassemblement"
        );
    }

    #[tokio::test]
    async fn une_annonce_arrivee_apres_le_rassemblement_atteint_la_boucle() {
        // Le cas qui motive tout ce chantier : le greffon parle **après** le
        // retour de `gather`. Avant, le socket cessait d'être lu et l'annonce
        // était perdue jusqu'au prochain redémarrage du service.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();

        // Le rendez-vous se termine sur un figé, sans que personne n'ait parlé.
        let (tx, mut rx) = canal();
        let g = gather(
            &listener,
            &["radio".to_string()],
            aucun_mort(),
            Duration::from_millis(200),
            &tx,
            &mut rx,
        )
        .await;
        assert_eq!(g.figes, vec!["radio".to_string()]);

        // Le socket, lui, reste lu : `gather` l'a pris par référence, le voici
        // confié à la tâche qui vivra autant que le processus. Le canal, lui,
        // est celui du rassemblement : un seul canal pour les deux étages.
        tokio::spawn(accept_forever(listener, tx));

        annonce(&register, r#"{"name":"radio","kinds":["source"],"admin":true}"#).await;
        let recue = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("l'annonce tardive doit atteindre la boucle principale")
            .unwrap();
        assert_eq!(recue.name, "radio");
        assert_eq!(recue.kinds, vec![PluginKind::Source]);
        assert!(recue.admin);
    }

    #[tokio::test]
    async fn une_annonce_prete_a_lecheance_nest_jamais_perdue() {
        // Épreuve du canal unique. Quand une annonce et l'échéance sont prêtes
        // au même instant, `tokio::select!` tire au hasard : une fois sur deux
        // l'échéance gagne. Avec un canal propre au rassemblement, détruit à
        // son retour, l'annonce partait alors avec le récepteur — et le SDK
        // n'annonçant qu'une seule fois, le greffon se croyait enregistré et
        // attendait le prochain redémarrage du service sans laisser de trace.
        //
        // Ici les deux étages partagent un seul canal : le tirage ne décide
        // plus que du chemin. Ou `gather` la consomme, ou elle **reste en
        // file** pour le câblage à chaud, et la boucle principale la câble un
        // instant plus tard. Le test affirme cette issue, pas un chemin.
        //
        // Horloge **simulée** : c'est ce qui rend la course reproductible. Avec
        // l'horloge réelle, les deux minuteurs n'expirent jamais sur le même
        // cran et le rendez-vous gagne toujours ; le test passerait alors aussi
        // bien avec le défaut qu'il est censé interdire. Sous l'horloge
        // simulée, c'est l'échéance qui gagne — c'est-à-dire exactement le
        // chemin sur lequel l'ancien montage perdait l'annonce.
        //
        // 200 tours et non un seul : l'ordre de réveil de deux minuteurs
        // expirés au même instant n'est garanti par rien, et le jour où il
        // change le test doit continuer de vérifier l'issue sur les deux
        // chemins plutôt que de tomber sur un ordre devenu faux.
        tokio::time::pause();

        let mut par_gather = 0usize;
        let mut restees_en_file = 0usize;
        for _ in 0..200 {
            let dir = tempfile::tempdir().unwrap();
            let register = dir.path().join("register.sock");
            let listener = UnixListener::bind(&register).unwrap();
            let (tx, mut rx) = canal();
            // Le greffon se connecte tout de suite — `gather` accepte, et sa
            // tâche de lecture attend — mais n'écrit sa ligne qu'à l'instant
            // **exact** de l'échéance. La tâche dépose donc l'annonce sur le
            // même cran d'horloge que l'expiration du rendez-vous, et les deux
            // bras du `select!` sont prêts au même sondage. C'est le chemin
            // complet, socket compris, et non un dépôt direct dans le canal.
            let r = register.clone();
            tokio::spawn(async move {
                let mut s = UnixStream::connect(&r).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                s.write_all(b"{\"name\":\"radio\",\"kinds\":[\"source\"]}\n").await.unwrap();
                s.shutdown().await.unwrap();
            });

            let g = gather(
                &listener,
                &["radio".to_string()],
                aucun_mort(),
                Duration::from_millis(100),
                &tx,
                &mut rx,
            )
            .await;

            if g.announcements.contains_key("radio") {
                par_gather += 1;
            } else {
                // L'échéance a gagné le tirage. L'annonce n'est pas perdue pour
                // autant : elle est dans la file — ou elle y arrive à l'instant
                // suivant, l'émetteur de la tâche de lecture étant toujours
                // vivant — et c'est la boucle principale qui la câblera à
                // chaud. C'est très exactement ce que l'ancien montage rendait
                // impossible : son récepteur mourait avec `gather`.
                let recue = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                    .await
                    .expect("l'annonce doit rester cablable apres le rendez-vous")
                    .expect("le canal des deux etages ne se ferme pas");
                assert_eq!(recue.name, "radio");
                restees_en_file += 1;
            }
        }
        // Le chemin qui perdait l'annonce doit avoir été emprunté, sinon ce test
        // ne prouve rien : c'est lui l'épreuve. S'il cesse de se produire — un
        // ordre de réveil qui change — mieux vaut un échec bruyant qu'un test
        // qui passe sans plus rien vérifier.
        assert!(
            restees_en_file > 0,
            "l'echeance n'a jamais gagne le tirage ({par_gather} par gather) : le chemin qui perdait l'annonce n'est plus reproduit"
        );
    }

    #[tokio::test]
    async fn une_connexion_muette_est_lachee_au_bout_du_delai() {
        // Sans délai de lecture, chaque connexion muette immobilisait une tâche
        // et un descripteur pour la vie du processus. Un greffon avec un bug de
        // reconnexion, frappant le socket une fois par seconde sans écrire,
        // finissait par épuiser les descripteurs : plus aucune annonce câblable
        // sur un appareil qu'on ne redémarre jamais.
        let (a, mut b) = tokio::net::UnixStream::pair().unwrap();
        let (tx, mut rx) = canal();
        tokio::spawn(lit_annonce(a, tx, Duration::from_millis(100)));

        // La connexion lâchée par la tâche se voit de l'autre bout : une
        // lecture à zéro octet, c'est-à-dire une fin de fichier.
        let mut tampon = [0u8; 1];
        let lu = tokio::time::timeout(Duration::from_secs(2), b.read(&mut tampon))
            .await
            .expect("une connexion muette doit etre lachee, pas tenue pour la vie du processus")
            .unwrap();
        assert_eq!(lu, 0, "fin de fichier : le coeur a rendu son descripteur");
        assert!(rx.try_recv().is_err(), "rien a cabler depuis une connexion muette");
    }

    #[tokio::test]
    async fn une_connexion_muette_ne_bloque_pas_les_annonces_tardives() {
        // Même blocage de tête que sur le rendez-vous, même correctif : sans la
        // tâche de lecture par connexion, la connexion silencieuse ci-dessous
        // retiendrait toutes les annonces suivantes pour toujours — et cette
        // boucle n'a plus d'échéance pour la débloquer.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Announcement>(4);
        tokio::spawn(accept_forever(listener, tx));

        let muet = UnixStream::connect(&register).await.unwrap();
        annonce(&register, r#"{"name":"radio","kinds":["source"]}"#).await;

        let recue = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("une connexion muette ne doit pas retenir les annonces")
            .unwrap();
        assert_eq!(recue.name, "radio");
        drop(muet);
    }

    #[tokio::test]
    async fn une_annonce_tardive_illisible_ne_ferme_pas_le_socket() {
        // Un binaire fautif ne doit pas priver les autres du câblage à chaud.
        let dir = tempfile::tempdir().unwrap();
        let register = dir.path().join("register.sock");
        let listener = UnixListener::bind(&register).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Announcement>(4);
        tokio::spawn(accept_forever(listener, tx));

        annonce(&register, "ceci n'est pas du json").await;
        annonce(&register, r#"{"name":"radio","kinds":["source"]}"#).await;

        let recue = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("le socket doit continuer d'accepter apres une ligne illisible")
            .unwrap();
        assert_eq!(recue.name, "radio", "seule l'annonce lisible doit remonter");
    }

    #[tokio::test]
    async fn lordre_des_metadata_suit_le_manifeste_pas_les_arrivees() {
        // La garantie etait acquise par construction (liste batie avant tout
        // lancement) ; elle est desormais maintenue par le code, donc testee.
        let mut announcements = HashMap::new();
        for nom in ["musicbrainz", "ouifm-metas", "radiofrance-metas"] {
            announcements.insert(
                nom.to_string(),
                Announcement {
                    name: nom.to_string(),
                    kinds: vec![PluginKind::Metadata],
                    admin: false,
                },
            );
        }
        announcements.insert(
            "radio".to_string(),
            Announcement { name: "radio".into(), kinds: vec![PluginKind::Source], admin: true },
        );
        let g = Gathered { announcements, ..Default::default() };

        // Ordre du manifeste, deliberement different de l'ordre alphabetique
        // et de tout ordre d'arrivee plausible.
        let manifeste = vec![
            "radio".to_string(),
            "ouifm-metas".to_string(),
            "radiofrance-metas".to_string(),
            "musicbrainz".to_string(),
        ];
        assert_eq!(
            metadata_order(&manifeste, &g),
            vec![
                "ouifm-metas".to_string(),
                "radiofrance-metas".to_string(),
                "musicbrainz".to_string()
            ]
        );
    }

    #[test]
    fn aucun_greffon_lance_ne_laisse_personne_de_vivant() {
        // `plugins.toml` vide, ou tous les executables introuvables : personne
        // ne s'annoncera jamais. C'est une erreur de configuration, et le coeur
        // refuse encore de demarrer dans ce seul cas.
        assert!(!un_greffon_vivant(&[], &Gathered::default()));
    }

    #[test]
    fn tous_les_greffons_morts_ne_laissent_personne_de_vivant() {
        // Lances, puis morts avant l'echeance : plus rien ne tourne, donc plus
        // rien ne peut s'annoncer a chaud. Meme refus.
        let lances = vec!["radio".to_string(), "console".to_string()];
        let g = Gathered { morts: lances.clone(), ..Default::default() };
        assert!(!un_greffon_vivant(&lances, &g));
    }

    #[test]
    fn un_greffon_fige_reste_un_processus_vivant() {
        // Le cas qui justifie tout le chantier : `files` tourne, il n'a rien dit
        // a l'echeance, il peut encore parler. Le coeur doit demarrer pour que la
        // page de statut le montre fige — un refus la supprimerait precisement
        // quand on veut la consulter.
        let lances = vec!["radio".to_string(), "files".to_string()];
        let g = Gathered {
            morts: vec!["radio".to_string()],
            figes: vec!["files".to_string()],
            ..Default::default()
        };
        assert!(un_greffon_vivant(&lances, &g));
    }
}
