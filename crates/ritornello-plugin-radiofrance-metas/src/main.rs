//! Plugin `metadata` : titres des stations Radio France, depuis leur direct.
//!
//! Pourquoi ce plugin existe : les flux de Radio France n'émettent **aucune**
//! métadonnée ICY — pas de `icy-metaint` du tout, mesuré sur FIP comme sur ses
//! webradios. Là où OUI FM annonce au moins un texte de remplissage, une
//! station Radio France configurée sur l'appareil n'affiche aujourd'hui rien.
//! Radio France expose en revanche le direct de chaque station, sans
//! authentification, avec titre et artiste **déjà séparés**.
//!
//! Ce point d'entrée est **privé et non documenté** (seule la liste des
//! stations l'est) : il peut changer, exiger une authentification ou disparaître
//! sans préavis. D'où trois règles tenues ici : l'interrogation vit dans son
//! propre processus et ne retarde jamais la lecture, son échec est silencieux à
//! l'écran, et le rythme est celui que le serveur annonce lui-même, avec un
//! recul progressif en cas d'échec — un appareil sans surveillance ne doit pas
//! marteler le serveur d'un tiers. Rien n'est mis en cache sur disque.

mod live;
mod table;

use anyhow::Result;
use live::Meta;
use ritornello_plugin_sdk::{MetadataPlugin, Runtime};
use ritornello_proto::{CoverRef, Enrichment, NowPlaying};
use serde_json::Value;
use std::path::PathBuf;
use table::Table;
use tokio::sync::mpsc;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// URL d'une identité de flux, si c'en est une.
///
/// Fonction pure : point d'entrée de données venues d'un autre processus, donc
/// l'endroit où une forme inattendue doit être écartée sans bruit.
fn url_du_flux(identity: &Value) -> Option<&str> {
    if identity.get("kind").and_then(Value::as_str)? != "stream" {
        return None;
    }
    let url = identity.get("url").and_then(Value::as_str)?;
    (!url.trim().is_empty()).then_some(url)
}

struct RadioFranceMetas {
    table: Table,
    /// Identité courante, réémise en écho dans chaque enrichissement.
    identite: Option<Value>,
    /// Station suivie : son identifiant, et la tâche qui l'interroge.
    ///
    /// L'interrogation vit dans une tâche et non dans le futur de
    /// `next_enrichment` : ce futur est abandonné dès qu'un `NowPlaying`
    /// arrive, ce qui remettrait le cycle à zéro à chaque changement d'état du
    /// cœur — et surtout ferait perdre le « dernier vu » qui évite de réémettre
    /// le même morceau à chaque interrogation.
    suivi: Option<(u32, tokio::task::JoinHandle<()>)>,
    metas_tx: mpsc::Sender<(u32, Meta)>,
    metas_rx: mpsc::Receiver<(u32, Meta)>,
}

impl RadioFranceMetas {
    fn new(table: Table) -> Self {
        let (metas_tx, metas_rx) = mpsc::channel(8);
        Self { table, identite: None, suivi: None, metas_tx, metas_rx }
    }

    /// Arrête le suivi en cours, s'il y en a un.
    fn arrete(&mut self) {
        if let Some((id, tache)) = self.suivi.take() {
            tracing::debug!("stopped following station {id}");
            tache.abort();
        }
    }

    /// Suit cette station, sauf si c'est déjà celle qu'on suit — auquel cas la
    /// tâche en place est conservée. C'est le cas de tous les changements de
    /// morceau sur une même station : la relancer perdrait son « dernier vu »,
    /// donc ferait réémettre le morceau en cours, et solliciterait un tiers
    /// hors du rythme qu'il a lui-même annoncé.
    fn suit(&mut self, id: u32, profil: String) {
        if self.suivi.as_ref().is_some_and(|(en_cours, _)| *en_cours == id) {
            return;
        }
        self.arrete();
        let tx = self.metas_tx.clone();
        let tache = tokio::spawn(live::suit(id, profil, tx));
        self.suivi = Some((id, tache));
    }
}

#[async_trait::async_trait]
impl MetadataPlugin for RadioFranceMetas {
    async fn now_playing(&mut self, np: NowPlaying) {
        // Reconnaissance puis mutation : les valeurs sont copiées avant de
        // toucher à `self`, la table étant empruntée à `self`.
        let reconnue = np
            .identity
            .as_ref()
            .and_then(url_du_flux)
            .and_then(|url| self.table.station_pour(url))
            .map(|s| (s.id, s.label.clone(), s.rules.clone()));
        match reconnue {
            Some((id, label, profil)) => {
                tracing::debug!("station recognized: {label} (id {id}, profile {profil})");
                self.identite = np.identity;
                self.suit(id, profil);
            }
            None => {
                // Arrêt, disque, ou station inconnue de la table : on se tait,
                // et surtout on arrête la tâche — une interrogation laissée en
                // route continuerait de solliciter un tiers pour une station
                // qui ne joue plus.
                self.identite = None;
                self.arrete();
            }
        }
    }

    // `..Default::default()` derrière un littéral pourtant complet : clippy le
    // dit sans effet (`needless_update`), et il a raison **aujourd'hui**. Ce
    // n'est pas de la redondance mais de la compatibilité ascendante — un
    // littéral qui se termine ainsi survit à l'ajout d'un champ dans la
    // structure, celui qui les énumère tous casse. Le dépôt a payé cette
    // leçon : un champ ajouté à une structure publique a cassé 44 littéraux
    // ailleurs, qu'un `cargo test -p` ne compile jamais. Quand clippy et la
    // compatibilité ascendante se contredisent ici, c'est la seconde qui
    // gagne, et la règle qui reçoit un `allow`.
    #[allow(clippy::needless_update)]
    async fn next_enrichment(&mut self) -> Enrichment {
        loop {
            // `recv` est annulable sans perte : si un `NowPlaying` arrive
            // d'abord, le runner abandonne ce futur sans qu'aucun relevé reçu
            // ne soit perdu. Tout ce qui suit sa résolution est synchrone,
            // donc hors d'atteinte d'une annulation.
            let Some((id, meta)) = self.metas_rx.recv().await else {
                // Impossible en pratique (le plugin garde un Sender).
                std::future::pending().await
            };
            // Relevé d'une station qu'on ne suit plus : il attendait en file au
            // moment du changement. Même principe que la péremption côté cœur.
            let suit_toujours = self.suivi.as_ref().is_some_and(|(en_cours, _)| *en_cours == id);
            if !suit_toujours {
                continue;
            }
            if let Some(identite) = &self.identite {
                return Enrichment {
                    identity: identite.clone(),
                    artist: meta.artist,
                    title: meta.title,
                    // Absent le plus souvent : le direct n'en donne pas, il se
                    // lit dans la grille, qui a fréquemment un morceau de
                    // retard (voir `live::album_dans_grille`).
                    album: meta.album,
                    duration_s: meta.duration_s,
                    cover: meta.cover.as_deref().map(|u| CoverRef::Url { url: live::url_pochette(u) }),
                    // Ce greffon lit le flux officiel de la station : il sait mieux que
                    // l'ICY, par construction. Il écrase, donc `fill_only` reste faux.
                    fill_only: false,
                    // L'écoulé est calculé **ici**, au moment d'émettre : c'est
                    // le seul instant où il est exact, et le cœur l'ancre à sa
                    // réception. Une horloge décalée ou un `startTime` dans le
                    // futur donnerait un écoulé négatif : `checked_sub` le
                    // ramène à « je ne sais pas » plutôt qu'à zéro, qui
                    // prétendrait savoir.
                    position_s: meta.start_time.and_then(|debut| {
                        let maintenant = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()?
                            .as_secs();
                        maintenant.checked_sub(debut).and_then(|e| u32::try_from(e).ok())
                    }),
                    ..Default::default()
                };
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let table_path = PathBuf::from(env_or(
        "RITORNELLO_RADIOFRANCE_METAS",
        "/etc/ritornello/radiofrance-metas.toml",
    ));
    let table = Table::load(&table_path);
    tracing::info!("{} station(s) known (bundled table + {})", table.stations.len(), table_path.display());
    Runtime::from_args()?.metadata(RadioFranceMetas::new(table))?.run().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// URL de flux réelle de FIP Groove, telle qu'un annuaire la publie.
    const URL: &str = "https://icecast.radiofrance.fr/fipgroove-midfi.mp3";
    /// Identifiant de station correspondant, relevé de la documentation.
    const ID: u32 = 66;

    fn identite_flux(url: &str) -> Value {
        json!({ "kind": "stream", "url": url })
    }

    /// Plugin dont le suivi est déjà déclaré : aucune tâche réseau n'est lancée
    /// dans les tests. **Aucun test ne touche le réseau.**
    fn plugin_suivant(id: u32) -> RadioFranceMetas {
        let mut p = RadioFranceMetas::new(Table::embarquee());
        // Une tâche inerte tient la place de l'interrogation HTTP.
        let tache = tokio::spawn(std::future::pending::<()>());
        p.suivi = Some((id, tache));
        p
    }

    #[test]
    fn reconnait_une_identite_de_flux() {
        assert_eq!(url_du_flux(&identite_flux(URL)), Some(URL));
    }

    #[test]
    fn ignore_ce_qui_nest_pas_un_flux() {
        assert!(url_du_flux(&json!({"kind": "disc", "toc": "3 1 2 3"})).is_none());
        assert!(url_du_flux(&json!({"kind": "stream"})).is_none());
        assert!(url_du_flux(&json!({"kind": "stream", "url": "  "})).is_none());
        assert!(url_du_flux(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn un_releve_devient_un_enrichissement_avec_echo_de_lidentite() {
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((
                ID,
                Meta {
                    artist: Some("Etta James".into()),
                    title: Some("Fire".into()),
                    album: Some("At Last!".into()),
                    duration_s: Some(197),
                    start_time: None,
                    cover: None,
                },
            ))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.identity, identite_flux(URL), "l'identite doit etre reemise en echo");
        assert_eq!(e.artist.as_deref(), Some("Etta James"));
        assert_eq!(e.title.as_deref(), Some("Fire"));
        assert_eq!(e.duration_s, Some(197));
        // L'album ne vient pas du direct mais de la grille, et il traverse
        // jusqu'à l'enrichissement — c'est ce que le cœur place dans
        // `morceau.album`, dont un afficheur peut faire une ligne.
        assert_eq!(e.album.as_deref(), Some("At Last!"));
    }

    #[tokio::test]
    async fn la_pochette_devient_une_url_composee_et_ecrase() {
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((ID, Meta { title: Some("Fire".into()), cover: Some("uuid-test".into()), ..Default::default() }))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(
            e.cover,
            Some(CoverRef::Url {
                url: "https://api.radiofrance.fr/v1/services/embed/image/uuid-test?preset=400x400".into()
            })
        );
        assert!(!e.fill_only, "ce greffon sait mieux que l'ICY, il doit ecraser");
    }

    #[tokio::test]
    async fn un_morceau_sans_album_reste_un_enrichissement_valable() {
        // Cas le plus courant : la grille a un morceau de retard. L'absence
        // d'album ne doit rien retenir du reste.
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)), ..Default::default() }).await;
        p.metas_tx
            .send((ID, Meta { title: Some("Fire".into()), ..Default::default() }))
            .await
            .unwrap();
        let e = p.next_enrichment().await;
        assert_eq!(e.title.as_deref(), Some("Fire"));
        assert_eq!(e.album, None);
    }

    #[tokio::test]
    async fn une_station_inconnue_de_la_table_ferme_le_suivi() {
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying {
            source: "radio".into(),
            identity: Some(identite_flux("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3")),
            ..Default::default()
        })
        .await;
        assert!(p.suivi.is_none(), "une interrogation laissee en route solliciterait un tiers pour rien");
        assert!(p.identite.is_none());
    }

    #[tokio::test]
    async fn larret_de_la_lecture_ferme_le_suivi() {
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: None, ..Default::default() }).await;
        assert!(p.suivi.is_none());
    }

    #[tokio::test]
    async fn une_identite_de_disque_ferme_le_suivi() {
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying {
            source: "cd".into(),
            identity: Some(json!({"kind": "disc", "toc": "3 150 22767 41887 63000", "track": 0})),
            ..Default::default()
        })
        .await;
        assert!(p.suivi.is_none(), "ce plugin ne traite pas les disques");
    }

    #[tokio::test]
    async fn rester_sur_la_meme_station_conserve_la_tache() {
        // Un changement de morceau donne une nouvelle identité mais la même
        // station : relancer la tâche perdrait son « dernier vu » et ferait
        // réémettre le morceau en cours.
        //
        // Ce test prouve **aussi** la correspondance de bout en bout, depuis la
        // table embarquée : le suivi en place porte l'identifiant de FIP Groove,
        // et si l'URL réelle ne se résolvait pas exactement sur lui, `suit`
        // abandonnerait cette tâche pour en lancer une autre — ce que
        // l'assertion ci-dessous refuse. Aucune requête n'est émise, pour cette
        // raison même.
        let mut p = plugin_suivant(ID);
        let avant = p.suivi.as_ref().map(|(id, t)| (*id, t.id()));
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)), ..Default::default() }).await;
        let apres = p.suivi.as_ref().map(|(id, t)| (*id, t.id()));
        assert_eq!(avant, apres, "la meme tache doit continuer");
    }

    #[tokio::test]
    async fn un_releve_dune_station_quon_ne_suit_plus_est_ecarte() {
        let mut p = plugin_suivant(ID);
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)), ..Default::default() }).await;
        // Relevé en file au moment du changement de station.
        p.metas_tx
            .send((99, Meta { title: Some("ancien".into()), ..Default::default() }))
            .await
            .unwrap();
        let r = tokio::time::timeout(std::time::Duration::from_millis(200), p.next_enrichment()).await;
        assert!(r.is_err(), "un releve hors sujet ne doit produire aucun enrichissement");
    }

    #[tokio::test]
    async fn une_table_vide_ne_suit_jamais_rien() {
        // Cas dégénéré, atteignable si la table embarquée devenait vide : le
        // plugin doit rester muet, jamais deviner un identifiant.
        let mut p = RadioFranceMetas::new(Table::default());
        p.now_playing(NowPlaying { source: "radio".into(), identity: Some(identite_flux(URL)), ..Default::default() }).await;
        assert!(p.suivi.is_none());
    }
}
