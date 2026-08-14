//! Plugin Source « cd » : présence du disque, lecture, piste courante, éjection.
//!
//! Il ne connaît **aucun** fournisseur de métadonnées. Ce qu'il sait du disque,
//! il le déclare dans l'identité du morceau (la TOC brute et l'index de piste) ;
//! artiste, album et titres viennent d'un plugin `metadata` — par exemple
//! `ritornello-plugin-musicbrainz` — que le cœur arbitre. Un appel réseau lent
//! ne vit donc plus dans le processus qui doit répondre aux commandes de piste.

mod cd;

use anyhow::Result;
use ritornello_plugin_sdk::{run_source_plugin, Notification, SourceOutcome, SourcePlugin};
use ritornello_proto::{SourceAction, View};
use std::path::PathBuf;
use tokio::sync::mpsc;

use ritornello_i18n::Catalog;

const CD_EN: &str = include_str!("locales/en.toml");

/// Résultat d'une lecture de TOC : époque de validité, TOC brute si lisible,
/// nombre de pistes.
type TocLue = (u64, Option<String>, usize);

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct CdSource {
    cd_dev: String,
    present: bool,
    track: i64,
    /// TOC brute du disque inséré (sortie de `cd-discid`), telle qu'elle part
    /// dans l'identité. `None` tant qu'elle n'a pas été lue, ou si elle est
    /// illisible.
    toc: Option<String>,
    /// TOC du disque précédent, seul moyen de distinguer un **clignotement de
    /// présence** du lecteur (même disque, la lecture continue) d'un **échange
    /// de disque** (rien ne peut plus jouer).
    toc_precedente: Option<String>,
    total_tracks: usize,
    /// Vrai si le plugin a demandé la lecture et ne l'a pas arrêtée depuis.
    ///
    /// Nécessaire pour l'identité : un disque **présent dans le tiroir** n'est
    /// pas un morceau **en cours de lecture**, et seul le second a des
    /// métadonnées à afficher. Sans cette distinction, insérer un disque sans
    /// rien lancer ferait interroger un service tiers pour rien.
    lecture: bool,
    epoch: u64,
    presence_rx: mpsc::Receiver<bool>,
    toc_tx: mpsc::Sender<TocLue>,
    toc_rx: mpsc::Receiver<TocLue>,
    catalog: Catalog,
    locales_root: PathBuf,
}

impl CdSource {
    /// Vue du plugin : ce que le disque lui-même permet de dire.
    ///
    /// `line2` porte « audio CD » — un remplissage, que le plugin déclare
    /// remplaçable (voir `issue`) : le cœur y écrira l'album s'il l'apprend d'un
    /// plugin `metadata`, et « audio CD » revient dès qu'il ne le sait plus.
    /// C'est ce qui évite deux lignes vides sur un disque absent de MusicBrainz,
    /// hors ligne, ou sans plugin `metadata` déclaré. `line3` reste libre pour la
    /// ligne artiste — titre.
    fn view(&self) -> View {
        if !self.present {
            return View {
                line1: "CD".into(),
                line2: self.catalog.get("no_disc").to_string(),
                line3: String::new(),
            };
        }
        let n = self.track.max(0) as usize;
        let line1 = if self.total_tracks > 0 {
            self.catalog
                .get("cd_n_of_total")
                .replace("{n}", &(n + 1).to_string())
                .replace("{total}", &self.total_tracks.to_string())
        } else {
            // TOC pas encore lue, ou illisible : le compte de pistes est inconnu.
            self.catalog.get("cd_track").replace("{n}", &(n + 1).to_string())
        };
        View { line1, line2: self.catalog.get("cd_audio").to_string(), line3: String::new() }
    }

    /// Issue complète : action, vue, et identité de ce qui joue.
    fn issue(&self, action: SourceAction) -> SourceOutcome {
        let sortie = SourceOutcome::new(action).with_view(self.view());
        // « audio CD » et « pas de disque » sont tous deux des remplissages :
        // l'album vaut mieux quand on le connaît, et l'étiquette revient sinon.
        let sortie = sortie.line2_replaceable();
        // The count is a property of the inserted disc, not of playback: it is
        // declared on every frame, 0 when no TOC is known (no disc, or the
        // TOC is still being read).
        let count = match &self.toc {
            Some(_) => u8::try_from(self.total_tracks).unwrap_or(255),
            None => 0,
        };
        let sortie = sortie.preset_count(count);
        match (self.lecture && self.present, &self.toc) {
            // La TOC désigne le disque, l'index désigne la piste : les deux sont
            // nécessaires, un changement de piste étant un changement de morceau.
            (true, Some(toc)) => {
                let sortie = sortie.plays(serde_json::json!({
                    "kind": "disc",
                    "toc": toc,
                    "tracks": self.total_tracks,
                    "track": self.track,
                }));
                // La piste en cours est la touche à mettre en évidence.
                match u8::try_from(self.track + 1) {
                    Ok(n) => sortie.preset(n),
                    Err(_) => sortie,
                }
            }
            // Rien ne joue, ou rien d'identifiable (TOC pas encore lue,
            // illisible, lecteur vide). On le dit : une identité partielle
            // ferait travailler les plugins pour rien.
            _ => sortie.plays_nothing(),
        }
    }

    fn spawn_toc_read(&self) {
        let cd_dev = self.cd_dev.clone();
        let tx = self.toc_tx.clone();
        let epoch = self.epoch;
        tokio::spawn(async move {
            let lue = tokio::task::spawn_blocking(move || {
                cd::read_toc(&cd_dev).and_then(|raw| {
                    let n = cd::toc_ntracks(&raw)?;
                    Ok((raw.trim().to_string(), n))
                })
            })
            .await;
            let resultat = match lue {
                Ok(Ok((raw, n))) => (epoch, Some(raw), n),
                Ok(Err(e)) => {
                    tracing::info!("TOC unreadable: {e}");
                    (epoch, None, 0)
                }
                Err(e) => {
                    tracing::warn!("TOC task interrupted: {e}");
                    (epoch, None, 0)
                }
            };
            let _ = tx.send(resultat).await;
        });
    }

    /// Remise à zéro sur changement de disque : l'époque invalide toute lecture
    /// de TOC encore en vol.
    fn oublie_le_disque(&mut self) {
        self.track = 0;
        // La dernière TOC **connue** est retenue : c'est elle qui dira, quand la
        // prochaine arrivera, si le disque a changé ou si le lecteur a simplement
        // cligné. Écraser avec `None` perdrait cette mémoire — un clignotement
        // produit deux changements de présence, donc deux passages ici, et le
        // second effacerait ce que le premier venait de retenir.
        if let Some(connue) = self.toc.take() {
            self.toc_precedente = Some(connue);
        }
        self.total_tracks = 0;
        self.epoch = self.epoch.wrapping_add(1);
    }
}

#[async_trait::async_trait]
impl SourcePlugin for CdSource {
    async fn activate(&mut self) -> SourceOutcome {
        self.lecture = self.present;
        if self.present {
            self.issue(SourceAction::Play { uri: "cdda://".into() })
        } else {
            self.issue(SourceAction::Noop)
        }
    }
    async fn deactivate(&mut self) -> SourceOutcome {
        self.lecture = false;
        SourceOutcome::new(SourceAction::Stop).plays_nothing()
    }
    async fn wake(&mut self) -> SourceOutcome {
        // Réveil : rafraîchir l'affichage (« pas de disque » / infos disque)
        // sans émettre de Play — le cd ne se lance pas tout seul, donc rien ne
        // joue et il n'y a aucune métadonnée à chercher.
        self.lecture = false;
        self.issue(SourceAction::Noop)
    }
    async fn select(&mut self, n: u8) -> SourceOutcome {
        if !self.present || n == 0 {
            return SourceOutcome::new(SourceAction::Noop);
        }
        if self.total_tracks > 0 && (n as usize) > self.total_tracks {
            return self.issue(SourceAction::Noop);
        }
        self.track = (n - 1) as i64;
        self.lecture = true;
        self.issue(SourceAction::Play { uri: format!("cdda://{n}") })
    }
    async fn next(&mut self) -> SourceOutcome {
        // Rien en lecture : `playlist-next` sur un mpv à l'arrêt ne charge rien,
        // donc sauter une piste n'a aucun sens. Surtout, il ne faut pas armer
        // `lecture` ici : cela déclarerait un morceau en cours sur un appareil
        // silencieux, ferait interroger un service tiers et afficherait un
        // artiste et un titre sans un son.
        if !self.lecture {
            return SourceOutcome::new(SourceAction::Noop);
        }
        // Le lecteur ne remonte pas l'index réel : on suit l'index demandé,
        // borné à la dernière piste connue (pas de rebouclage).
        if self.total_tracks > 0 {
            self.track = (self.track + 1).min(self.total_tracks as i64 - 1);
        }
        self.issue(SourceAction::PlayerNext)
    }
    async fn prev(&mut self) -> SourceOutcome {
        // Voir `next` : même garde, même raison.
        if !self.lecture {
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.track = (self.track - 1).max(0);
        self.issue(SourceAction::PlayerPrev)
    }
    async fn stop(&mut self) -> SourceOutcome {
        // Arrêt décidé par le cœur, que la Source n'aurait pas su autrement.
        self.lecture = false;
        SourceOutcome::new(SourceAction::Noop).plays_nothing()
    }
    async fn player_track(&mut self, n: i64) -> SourceOutcome {
        // Le disque avance seul en fin de piste : c'est le **seul** chemin par
        // lequel le plugin l'apprend, mpv ne remontant pas l'index autrement.
        // Sans cela, l'affichage et les métadonnées restaient sur la piste
        // précédente jusqu'à ce que l'utilisateur touche une touche.
        if !self.present || n < 0 {
            return SourceOutcome::new(SourceAction::Noop);
        }
        if self.total_tracks > 0 && n >= self.total_tracks as i64 {
            // Index hors du disque : ne pas suivre une valeur qu'on sait fausse.
            return SourceOutcome::new(SourceAction::Noop);
        }
        self.track = n;
        // Le lecteur annonce une avance de piste : il joue donc, quoi que le
        // plugin ait cru jusqu'ici. C'est aussi ce qui répare l'état après un
        // clignotement de présence du lecteur.
        self.lecture = true;
        self.issue(SourceAction::Noop)
    }
    async fn eject(&mut self) -> SourceOutcome {
        let cd_dev = self.cd_dev.clone();
        // `spawn_blocking` seul suffit : la commande `eject` bloque le temps
        // que le tiroir s'ouvre, et la réponse au cœur ne l'attend pas. Le
        // `JoinHandle` est lâché sciemment — `cd::eject` journalise lui-même
        // ses échecs, il n'y a rien à récolter ici.
        tokio::task::spawn_blocking(move || cd::eject(&cd_dev));
        self.present = false;
        self.lecture = false;
        self.oublie_le_disque();
        self.issue(SourceAction::Stop)
    }

    async fn set_locale(&mut self, locale: String) {
        self.catalog = Catalog::load("cd", &locale, &self.locales_root, CD_EN);
    }

    async fn poll_notification(&mut self) -> Option<Notification> {
        tokio::select! {
            presence = self.presence_rx.recv() => {
                let present = presence?;
                self.present = present;
                // `lecture` n'est **pas** touchée ici, et c'est délibéré :
                // `issue` exige déjà `lecture && present`, donc un disque parti
                // n'annonce rien. Le remettre à faux briserait le cas du
                // clignotement de présence — le lecteur rapporte
                // transitoirement « pas de disque » alors que mpv lit toujours,
                // et les métadonnées du disque resteraient éteintes jusqu'à la
                // fin, sans que rien ne se répare. Le cas de l'échange de disque
                // est traité à l'arrivée de la nouvelle TOC : c'est le premier
                // instant où on peut le distinguer d'un clignotement.
                self.oublie_le_disque();
                if present {
                    self.spawn_toc_read();
                }
                // Un disque inséré ne joue pas encore : `plays_nothing`, via
                // `issue`, qui tient compte de `lecture`.
                Some(self.notification())
            }
            toc = self.toc_rx.recv() => {
                let (epoch, toc, total_tracks) = toc?;
                if epoch != self.epoch {
                    return None;
                }
                self.total_tracks = total_tracks;
                // Disque **différent** du précédent : il a été échangé, donc rien
                // ne peut être en lecture — mpv ne joue plus ce qu'il jouait, et
                // aucun `Play` n'a été émis pour ce disque-ci. Même TOC : c'était
                // un clignotement de présence du lecteur, l'état de lecture est
                // conservé et les métadonnées reviennent.
                //
                // La comparaison n'a lieu que si une TOC précédente est connue :
                // au premier disque, elle vaut `None` et il ne faut surtout pas
                // éteindre une lecture que l'utilisateur vient de lancer.
                if let Some(precedente) = &self.toc_precedente {
                    if Some(precedente) != toc.as_ref() {
                        self.lecture = false;
                    }
                }
                self.toc = toc;
                // Arrivée différée de la TOC : c'est l'instant où le morceau
                // devient identifiable, donc où les plugins `metadata` peuvent
                // enfin travailler — d'où l'identité dans la notification.
                Some(self.notification())
            }
        }
    }
}

impl CdSource {
    /// Notification spontanée portant la vue **et** l'identité, construite
    /// depuis la même issue que les réponses aux requêtes (pour ne pas avoir
    /// deux règles d'identité à garder cohérentes).
    fn notification(&self) -> Notification {
        let issue = self.issue(SourceAction::Noop);
        Notification {
            view: issue.view,
            identity: issue.identity,
            line2_replaceable: issue.line2_replaceable,
            // Jamais éphémère : ce que le cd rapporte (disque inséré, TOC lue,
            // piste changée) décrit l'état durable de l'appareil.
            transient: false,
            preset: issue.preset,
            // The TOC can arrive after activation (async read): without this,
            // the count declared at activation (0, TOC unknown yet) would
            // never be corrected once the disc is actually readable.
            preset_count: issue.preset_count,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let socket_path = ritornello_plugin_sdk::socket_path();
    let cd_dev = env_or("RITORNELLO_CD_DEV", "/dev/sr0");

    let (presence_tx, presence_rx) = mpsc::channel(8);
    tokio::spawn(cd::watch(PathBuf::from(cd_dev.clone()), presence_tx));

    let (toc_tx, toc_rx) = mpsc::channel::<TocLue>(4);

    let locales_root = PathBuf::from(env_or("RITORNELLO_LOCALES", "/etc/ritornello/locales"));

    let source = CdSource {
        cd_dev,
        present: false,
        track: 0,
        toc: None,
        toc_precedente: None,
        total_tracks: 0,
        lecture: false,
        epoch: 0,
        presence_rx,
        toc_tx,
        toc_rx,
        catalog: Catalog::load("cd", "en", &locales_root, CD_EN),
        locales_root,
    };
    run_source_plugin(source, &socket_path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::IdentityUpdate;

    fn source_with_channels() -> (CdSource, mpsc::Sender<bool>, mpsc::Sender<TocLue>) {
        let (presence_tx, presence_rx) = mpsc::channel(8);
        let (toc_tx, toc_rx) = mpsc::channel(4);
        let source = CdSource {
            cd_dev: "/dev/sr0".into(),
            present: true,
            track: 0,
            toc: None,
            toc_precedente: None,
            total_tracks: 0,
            lecture: false,
            epoch: 5,
            presence_rx,
            toc_tx: toc_tx.clone(),
            toc_rx,
            catalog: Catalog::load("cd", "en", std::path::Path::new("/nonexistent"), CD_EN),
            locales_root: std::path::PathBuf::from("/nonexistent"),
        };
        (source, presence_tx, toc_tx)
    }

    /// Disque lu et en lecture : l'état où l'identité est complète.
    fn source_en_lecture() -> CdSource {
        let (mut source, _p, _t) = source_with_channels();
        source.toc = Some("3 150 22767 41887 63000".into());
        source.total_tracks = 3;
        source.lecture = true;
        source
    }

    #[tokio::test]
    async fn resultat_perime_ignore_resultat_frais_applique() {
        let (mut source, _presence_tx, toc_tx) = source_with_channels();
        // Un resultat perime (epoch 4, alors que source.epoch == 5) est ignore.
        toc_tx.send((4, Some("9 1 2 3".into()), 99)).await.unwrap();
        let n = source.poll_notification().await;
        assert!(n.is_none(), "un resultat perime ne doit produire aucune notification");
        assert_eq!(source.total_tracks, 0, "l'etat ne doit pas etre modifie par un resultat perime");
        assert!(source.toc.is_none());

        // Un resultat a jour (epoch 5) est applique.
        toc_tx.send((5, Some("12 150 200".into()), 12)).await.unwrap();
        let n = source.poll_notification().await;
        assert!(n.is_some());
        assert_eq!(source.total_tracks, 12);
    }

    #[tokio::test]
    async fn larrivee_de_la_toc_rend_le_morceau_identifiable() {
        // C'est l'instant qui débloque les plugins `metadata` : avant, le disque
        // joue mais rien ne l'identifie.
        let (mut source, _p, toc_tx) = source_with_channels();
        source.lecture = true;
        let avant = source.issue(SourceAction::Noop);
        assert_eq!(avant.identity, Some(IdentityUpdate::Nothing), "sans TOC, rien d'identifiable");

        toc_tx.send((5, Some("3 150 22767 41887 63000".into()), 3)).await.unwrap();
        let n = source.poll_notification().await.expect("notification attendue");
        assert_eq!(
            n.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "disc",
                "toc": "3 150 22767 41887 63000",
                "tracks": 3,
                "track": 0,
            })))
        );
        // La TOC arrive en asynchrone, apres l'activation qui avait declare
        // 0 (compte inconnu) : la notification doit corriger le compte,
        // sinon la fenetre de numeros affichee reste fausse.
        assert_eq!(n.preset_count, Some(3));
    }

    #[tokio::test]
    async fn un_disque_insere_mais_non_lu_nest_pas_un_morceau() {
        let (mut source, presence_tx, _t) = source_with_channels();
        source.present = false;
        presence_tx.send(true).await.unwrap();
        let n = source.poll_notification().await.expect("notification attendue");
        assert!(source.present);
        assert_eq!(
            n.identity,
            Some(IdentityUpdate::Nothing),
            "le cd ne se lance pas tout seul : rien ne joue, donc rien a enrichir"
        );
    }

    #[tokio::test]
    async fn changer_de_piste_change_lidentite() {
        let mut source = source_en_lecture();
        let out = source.next().await;
        assert_eq!(out.action, SourceAction::PlayerNext);
        let attendue = serde_json::json!({
            "kind": "disc",
            "toc": "3 150 22767 41887 63000",
            "tracks": 3,
            "track": 1,
        });
        assert_eq!(out.identity, Some(IdentityUpdate::Playing(attendue)));
    }

    #[tokio::test]
    async fn ejecter_declare_que_plus_rien_ne_joue() {
        let mut source = source_en_lecture();
        let out = source.eject().await;
        assert_eq!(out.action, SourceAction::Stop);
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
        assert!(source.toc.is_none(), "la TOC du disque ejecte ne doit pas survivre");
    }

    #[tokio::test]
    async fn la_ligne2_porte_une_etiquette_declaree_remplacable() {
        // Deux exigences à la fois : ne jamais laisser deux lignes vides quand
        // aucun album n'est connu (disque absent de MusicBrainz, hors ligne, ou
        // aucun plugin `metadata` declare), et laisser le cœur y ecrire l'album
        // quand il l'apprend. D'ou une etiquette **declaree remplacable**,
        // plutot qu'une ligne vide qui demanderait l'album en se taisant.
        let source = source_en_lecture();
        let v = source.view();
        assert_eq!(v.line1, "CD 1/3");
        assert_eq!(v.line2, "audio CD");
        assert_eq!(v.line3, "", "place laissee a la ligne artiste - titre");
        assert!(
            source.issue(SourceAction::Noop).line2_replaceable,
            "sans cette declaration, l'album n'aurait nulle part ou s'afficher"
        );
    }

    #[tokio::test]
    async fn sauter_une_piste_sans_lecture_en_cours_ne_declare_rien() {
        // Disque lu, mais rien lance : `playlist-next` sur un mpv a l'arret ne
        // charge rien. Declarer une lecture ici ferait interroger un service
        // tiers et afficher un artiste et un titre sur un appareil silencieux.
        let (mut source, _p, _t) = source_with_channels();
        source.toc = Some("3 150 22767 41887 63000".into());
        source.total_tracks = 3;
        source.lecture = false;

        let out = source.next().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert!(out.identity.is_none(), "rien ne doit etre annonce aux plugins metadata");
        assert_eq!(source.track, 0, "l'index ne doit pas bouger");

        let out = source.prev().await;
        assert_eq!(out.action, SourceAction::Noop);
        assert!(out.identity.is_none());
    }

    #[tokio::test]
    async fn un_arret_decide_par_le_coeur_remet_letat_de_lecture_a_jour() {
        // `Command::Stop` ne traverse pas la Source : sans cette notification,
        // `lecture` resterait vraie et le plugin annoncerait plus tard des
        // metadonnees pour un morceau a l'arret.
        let mut source = source_en_lecture();
        let out = source.stop().await;
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
        assert!(!source.lecture);
        // Et la consequence : plus rien n'est annonce, meme a l'arrivee d'une TOC.
        assert_eq!(source.issue(SourceAction::Noop).identity, Some(IdentityUpdate::Nothing));
    }

    #[tokio::test]
    async fn lavance_automatique_de_piste_met_a_jour_vue_et_identite() {
        // Fin de piste : le disque avance sans qu'aucune touche soit pressée.
        // Avant cette notification, l'affichage et les métadonnées restaient sur
        // la piste précédente jusqu'à la prochaine commande de l'utilisateur.
        let mut source = source_en_lecture();
        let out = source.player_track(2).await;
        assert_eq!(source.track, 2);
        assert_eq!(out.view.expect("vue attendue").line1, "CD 3/3");
        assert_eq!(
            out.identity,
            Some(IdentityUpdate::Playing(serde_json::json!({
                "kind": "disc",
                "toc": "3 150 22767 41887 63000",
                "tracks": 3,
                "track": 2,
            })))
        );
    }

    #[tokio::test]
    async fn la_piste_en_lecture_est_declaree_comme_touche_active() {
        // La piste en cours (0-indexee en interne) est la touche que l'IHM
        // met en evidence, quel que soit son numero.
        let mut source = source_en_lecture();
        let out = source.player_track(2).await;
        assert_eq!(out.preset, Some(3));
        // Sans lecture, aucune touche a mettre en evidence.
        source.lecture = false;
        assert_eq!(source.issue(SourceAction::Noop).preset, None);
        // Au-dela de la 9e piste, la touche correspond toujours : le +10 de
        // la telecommande et la fenetre web permettent d'y acceder.
        source.lecture = true;
        source.total_tracks = 12;
        source.track = 10;
        assert_eq!(source.issue(SourceAction::Noop).preset, Some(11));
    }

    #[test]
    fn le_compte_de_pistes_suit_la_toc() {
        // TOC connue -> total des pistes ; pas de TOC (pas de disque, ou lecture
        // en cours de la TOC) -> 0, « rien a numeroter ».
        let mut source = source_en_lecture();
        source.total_tracks = 12;
        assert_eq!(source.issue(SourceAction::Noop).preset_count, Some(12));

        source.toc = None;
        assert_eq!(source.issue(SourceAction::Noop).preset_count, Some(0));
    }

    #[tokio::test]
    async fn une_avance_de_piste_hors_disque_ou_sans_disque_est_ignoree() {
        let mut source = source_en_lecture();
        // Index au-delà du nombre de pistes connu : valeur qu'on sait fausse.
        let out = source.player_track(9).await;
        assert!(out.identity.is_none());
        assert_eq!(source.track, 0, "l'index ne doit pas suivre une valeur fausse");
        // `-1` est ce que mpv rapporte quand il n'y a pas de chapitre.
        assert!(source.player_track(-1).await.identity.is_none());
        // Sans disque, rien à suivre.
        source.present = false;
        assert!(source.player_track(1).await.identity.is_none());
    }

    #[tokio::test]
    async fn une_avance_de_piste_atteste_la_lecture() {
        // Le lecteur annonce l'avance : il joue donc, quoi que le plugin ait cru.
        // C'est ce qui répare l'état après un clignotement de présence.
        let (mut source, _p, _t) = source_with_channels();
        source.toc = Some("3 150 22767 41887 63000".into());
        source.total_tracks = 3;
        source.lecture = false;
        let out = source.player_track(1).await;
        assert!(source.lecture);
        assert!(matches!(out.identity, Some(IdentityUpdate::Playing(_))));
    }

    #[tokio::test]
    async fn un_disque_echange_neteint_pas_la_lecture_par_erreur() {
        // Distinction impossible avant l'arrivée de la nouvelle TOC : même TOC,
        // c'était un clignotement du lecteur et la lecture continue ; TOC
        // différente, le disque a été échangé et rien ne peut jouer — aucun
        // `Play` n'a été émis pour celui-ci.
        let mut source = source_en_lecture();
        let (toc_tx, toc_rx) = mpsc::channel(4);
        source.toc_tx = toc_tx.clone();
        source.toc_rx = toc_rx;
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;

        // Le disque est retiré puis un **autre** est inséré.
        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        let epoch = source.epoch;
        toc_tx.send((epoch, Some("12 150 200 300".into()), 12)).await.unwrap();
        let n = source.poll_notification().await.expect("notification");

        assert!(!source.lecture, "rien ne joue : aucun Play n'a ete emis pour ce disque");
        assert_eq!(
            n.identity,
            Some(IdentityUpdate::Nothing),
            "annoncer une identite ferait interroger un tiers pour un disque a l'arret"
        );
    }

    #[tokio::test]
    async fn le_meme_disque_relu_apres_un_clignotement_garde_sa_lecture() {
        let mut source = source_en_lecture();
        let toc_courante = source.toc.clone().expect("toc posee par le montage");
        let (toc_tx, toc_rx) = mpsc::channel(4);
        source.toc_tx = toc_tx.clone();
        source.toc_rx = toc_rx;
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;

        presence_tx.send(false).await.unwrap();
        source.poll_notification().await;
        presence_tx.send(true).await.unwrap();
        source.poll_notification().await;
        let epoch = source.epoch;
        toc_tx.send((epoch, Some(toc_courante), 3)).await.unwrap();
        let n = source.poll_notification().await.expect("notification");

        assert!(source.lecture, "le meme disque : la lecture n'a jamais cesse");
        assert!(
            matches!(n.identity, Some(IdentityUpdate::Playing(_))),
            "les metadonnees doivent revenir apres un clignotement"
        );
    }

    #[tokio::test]
    async fn un_clignotement_de_presence_neteint_pas_les_metadonnees() {
        // Le lecteur peut rapporter transitoirement « pas de disque » alors que
        // mpv lit toujours. Avant correction, `lecture` etait remise a faux sur
        // le retour de presence et ne se rearmait jamais : les metadonnees du
        // disque restaient eteintes jusqu'a la fin, sans rien pour les rallumer.
        let mut source = source_en_lecture();
        let (presence_tx, presence_rx) = mpsc::channel(8);
        source.presence_rx = presence_rx;
        presence_tx.send(false).await.unwrap();
        let n = source.poll_notification().await.expect("notification");
        assert_eq!(n.identity, Some(IdentityUpdate::Nothing), "disque parti : rien ne joue");

        presence_tx.send(true).await.unwrap();
        let _ = source.poll_notification().await;
        assert!(source.lecture, "la lecture ne doit pas avoir ete eteinte par le clignotement");
    }

    #[tokio::test]
    async fn view_utilise_le_catalogue_apres_set_locale() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("cd")).unwrap();
        std::fs::write(dir.path().join("cd/fr.toml"), "no_disc = \"PAS DE DISQUE\"\n").unwrap();

        let (mut source, _presence_tx, _toc_tx) = source_with_channels();
        source.present = false;
        source.locales_root = dir.path().to_path_buf();
        source.set_locale("fr".into()).await;
        assert_eq!(source.view().line2, "PAS DE DISQUE");
    }

    #[tokio::test]
    async fn next_incremente_borne_et_renvoie_une_vue() {
        let mut source = source_en_lecture();
        source.track = 0;
        let out = source.next().await;
        assert_eq!(out.action, SourceAction::PlayerNext);
        assert!(out.view.is_some(), "la vue doit suivre la piste");
        assert_eq!(source.track, 1);
        // Bornage haut : sur la dernière piste, next ne reboucle pas.
        source.track = 2;
        let _ = source.next().await;
        assert_eq!(source.track, 2);
    }

    #[tokio::test]
    async fn prev_decremente_borne_a_zero() {
        let mut source = source_en_lecture();
        source.track = 1;
        let out = source.prev().await;
        assert_eq!(out.action, SourceAction::PlayerPrev);
        assert!(out.view.is_some());
        assert_eq!(source.track, 0);
        // Bornage bas : sur la première piste, prev reste à 0.
        let _ = source.prev().await;
        assert_eq!(source.track, 0);
    }

    #[tokio::test]
    async fn wake_rafraichit_sans_jouer() {
        let (mut source, _p, _t) = source_with_channels();
        source.present = false;
        let out = source.wake().await;
        assert_eq!(out.action, SourceAction::Noop, "cd ne doit pas jouer au réveil");
        assert!(out.view.is_some());
        assert_eq!(out.identity, Some(IdentityUpdate::Nothing));
    }

    #[test]
    fn en_embarque_cd_est_non_vide() {
        assert!(!ritornello_i18n::try_parse(CD_EN).unwrap().is_empty());
    }
}
