use crate::types::Event;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};

pub struct MpvIpc {
    writer: Mutex<OwnedWriteHalf>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>,
    next_id: AtomicU64,
}

impl MpvIpc {
    pub fn from_stream(stream: UnixStream, events: mpsc::Sender<Event>) -> Arc<Self> {
        let (read, write) = stream.into_split();
        let ipc = Arc::new(Self {
            writer: Mutex::new(write),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
        });
        let pending = ipc.pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let v = match serde_json::from_str::<Value>(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("non-JSON mpv line ignored: {e}");
                        continue;
                    }
                };
                if let Some(id) = v.get("request_id").and_then(Value::as_u64) {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let res = if v["error"] == json!("success") {
                            Ok(v.get("data").cloned().unwrap_or(Value::Null))
                        } else {
                            Err(anyhow::anyhow!("mpv: {}", v["error"]))
                        };
                        let _ = tx.send(res);
                    }
                } else if v["event"] == json!("property-change") {
                    let ev = match (v["name"].as_str(), &v["data"]) {
                        (Some("media-title"), Value::String(t)) => Some(Event::Title(t.clone())),
                        (Some("metadata"), data) => icy_title(data).map(Event::IcyTitle),
                        (Some("idle-active"), Value::Bool(true)) => Some(Event::PlaybackIdle),
                        (Some("idle-active"), Value::Bool(false)) => Some(Event::PlaybackActive),
                        // Deux propriétés pour un même fait, l'avance de piste :
                        // mpv expose les pistes d'un CD comme des entrées de
                        // liste de lecture ou comme des chapitres selon la
                        // façon dont le disque a été ouvert (`cdda://` entier
                        // ou `cdda://<piste>`). Une seule des deux parle donc à
                        // la fois, et le cœur relaie la même chose dans les deux
                        // cas — c'est la Source qui sait ce que « piste n »
                        // signifie. Un index négatif (mpv dit `-1` quand il n'y
                        // a pas de chapitre) est transmis tel quel et écarté par
                        // la Source.
                        (Some("playlist-pos") | Some("chapter"), Value::Number(n)) => {
                            n.as_i64().map(Event::TrackChanged)
                        }
                        _ => None,
                    };
                    if let Some(ev) = ev {
                        // `mpsc` sans perte : canal plein = contre-pression sur
                        // cette pompe (la lecture de la socket attend), jamais
                        // d'événement jeté. Récepteur disparu = boucle du cœur
                        // finie, plus personne à servir.
                        if events.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
            }
            tracing::warn!("mpv socket closed");
        });
        ipc
    }

    pub async fn command(&self, args: &[Value]) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let msg = json!({ "command": args, "request_id": id });
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = w.write_all(format!("{msg}\n").as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(e.into());
            }
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => bail!("mpv: response abandoned"),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("mpv: command timeout")
            }
        }
    }

    pub async fn observe(&self, name: &str) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.command(&[json!("observe_property"), json!(id), json!(name)]).await?;
        Ok(())
    }
}

/// Extrait le titre annoncé par le flux du contenu de la propriété `metadata`
/// de mpv. Fonction pure, testable sur une capture réelle.
///
/// La clé est cherchée **sans égard à la casse** : mpv recopie les noms de
/// champs tels que la station les envoie, et l'en-tête ICY apparaît selon les
/// serveurs en `icy-title`, `Icy-Title` ou `ICY-TITLE`.
///
/// Une valeur vide ou blanche donne `None`, donc aucun événement : plusieurs
/// stations mesurées émettent un `StreamTitle` vide entre deux morceaux (et OUI
/// FM y met un texte de remplissage). Effacer l'affichage à chaque trou
/// laisserait la ligne clignoter, alors que le changement de morceau, lui,
/// remet déjà l'ardoise à zéro côté cœur.
pub fn icy_title(data: &Value) -> Option<String> {
    let map = data.as_object()?;
    let brut = map
        .iter()
        .find(|(cle, _)| cle.eq_ignore_ascii_case("icy-title"))
        .and_then(|(_, valeur)| valeur.as_str())?;
    let elague = brut.trim();
    (!elague.is_empty()).then(|| elague.to_string())
}

pub struct MpvPlayer {
    ipc: Arc<MpvIpc>,
}

/// Tampon de sortie audio, en secondes. **On reprend le défaut de mpv**, donc
/// ce module ne change rien au comportement tant que la variable n'est pas
/// définie : la cause des microcoupures observées n'est pas établie, et élargir
/// d'office aurait masqué le diagnostic plutôt que de le faire. La molette
/// existe parce que la bonne valeur dépend de la machine — sur un Pi 2, une
/// hausse de charge peut faire manquer une échéance d'écriture ALSA, ce qui
/// s'entend comme une microcoupure, et monter à 0,5 s est alors le premier
/// essai. Le coût est une latence d'autant sur la prise en compte du volume ou
/// du muet, imperceptible pour de la radio.
pub const AUDIO_BUFFER_DEFAUT: f64 = 0.2;

/// Borne haute imposée par mpv à `--audio-buffer`.
const AUDIO_BUFFER_MAX: f64 = 10.0;

/// Avance de lecture, en secondes. **On reprend le défaut de mpv**, pour la
/// même raison que le tampon de sortie : ne rien changer sans avoir mesuré.
/// Une seconde est pourtant mince pour un flux internet — la moindre gigue
/// réseau vide l'avance et mpv met la lecture en pause le temps de se remplir —
/// donc c'est la molette à tourner en premier sur une liaison capricieuse.
/// Dix secondes de MP3 à 128 kbit/s pèsent environ 160 Ko, négligeable même
/// sur 1 Go de RAM.
pub const READAHEAD_DEFAUT: f64 = 1.0;

/// Borne haute retenue ici : au-delà, le tampon coûte de la mémoire sans
/// bénéfice audible, et retarde la prise en compte d'un changement de station.
const READAHEAD_MAX: f64 = 120.0;

/// Lit une durée fournie par l'environnement. Variable absente : le défaut, en
/// silence. Valeur illisible, négative ou hors bornes : le défaut **avec** un
/// avertissement, plutôt qu'un échec de démarrage — un appareil muet parce
/// qu'une variable est mal écrite serait un pire résultat qu'un réglage par
/// défaut.
fn duree_reglee(brut: Option<&str>, defaut: f64, max: f64, quoi: &str) -> f64 {
    let Some(brut) = brut else { return defaut };
    match brut.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && (0.0..=max).contains(&v) => v,
        Ok(v) => {
            tracing::warn!("{quoi}={v} out of bounds (0..={max}), keeping {defaut}");
            defaut
        }
        Err(e) => {
            tracing::warn!("{quoi}={brut:?} unreadable ({e}), keeping {defaut}");
            defaut
        }
    }
}

/// Tampon de sortie retenu, d'après `RITORNELLO_AUDIO_BUFFER` s'il est défini.
pub fn audio_buffer_regle(brut: Option<&str>) -> f64 {
    duree_reglee(brut, AUDIO_BUFFER_DEFAUT, AUDIO_BUFFER_MAX, "RITORNELLO_AUDIO_BUFFER")
}

/// Avance de lecture retenue, d'après `RITORNELLO_NETWORK_READAHEAD`.
pub fn readahead_regle(brut: Option<&str>) -> f64 {
    duree_reglee(brut, READAHEAD_DEFAUT, READAHEAD_MAX, "RITORNELLO_NETWORK_READAHEAD")
}

/// Arguments de lancement de mpv. Fonction pure, séparée de `start` pour être
/// testable sans lancer de processus.
pub fn mpv_args(socket: &Path, cd_dev: &str, audio_buffer: f64, readahead: f64) -> Vec<String> {
    vec![
        "--idle=yes".to_string(),
        "--no-video".to_string(),
        "--no-terminal".to_string(),
        format!("--input-ipc-server={}", socket.display()),
        format!("--cdda-device={cd_dev}"),
        format!("--audio-buffer={audio_buffer}"),
        format!("--demuxer-readahead-secs={readahead}"),
    ]
}

/// Propriétés que le cœur demande à mpv de pousser. `metadata` porte l'en-tête
/// ICY reçu de la station (clé `icy-title`), seule source de titre disponible
/// pour une radio sans plugin `metadata` dédié.
const OBSERVEES: [&str; 5] =
    ["media-title", "metadata", "idle-active", "playlist-pos", "chapter"];

/// Lance mpv en démon idle et s'y connecte. Le Child est rendu à l'appelant :
/// s'il meurt, main quitte et systemd relance tout le service.
pub async fn start(
    mpv_bin: &str,
    socket: &Path,
    cd_dev: &str,
    audio_buffer: f64,
    readahead: f64,
    events: mpsc::Sender<Event>,
) -> Result<(MpvPlayer, tokio::process::Child)> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(socket);
    let child = tokio::process::Command::new(mpv_bin)
        .args(mpv_args(socket, cd_dev, audio_buffer, readahead))
        .kill_on_drop(true)
        .spawn()
        .context("starting mpv")?;

    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket).await {
            stream = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stream = stream.context("connecting to mpv socket (10 s)")?;
    let ipc = MpvIpc::from_stream(stream, events);
    for propriete in OBSERVEES {
        ipc.observe(propriete).await?;
    }
    Ok((MpvPlayer { ipc }, child))
}

#[async_trait::async_trait]
impl super::Player for MpvPlayer {
    async fn play(&self, uri: &str) -> Result<()> {
        self.ipc.command(&[json!("loadfile"), json!(uri), json!("replace")]).await?;
        self.ipc.command(&[json!("set_property"), json!("pause"), json!(false)]).await?;
        Ok(())
    }
    async fn stop(&self) -> Result<()> {
        self.ipc.command(&[json!("stop")]).await?;
        Ok(())
    }
    async fn toggle_pause(&self) -> Result<()> {
        self.ipc.command(&[json!("cycle"), json!("pause")]).await?;
        Ok(())
    }
    async fn next(&self) -> Result<()> {
        self.ipc.command(&[json!("playlist-next")]).await?;
        Ok(())
    }
    async fn prev(&self) -> Result<()> {
        self.ipc.command(&[json!("playlist-prev")]).await?;
        Ok(())
    }
    async fn set_playlist_pos(&self, n: i64) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("playlist-pos"), json!(n)]).await?;
        Ok(())
    }
    async fn set_volume(&self, volume: u8) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("volume"), json!(volume)]).await?;
        Ok(())
    }
    async fn set_mute(&self, mute: bool) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("mute"), json!(mute)]).await?;
        Ok(())
    }
    async fn set_audio_device(&self, device: &str) -> Result<()> {
        self.ipc.command(&[json!("set_property"), json!("audio-device"), json!(device)]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Event;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn command_recoit_la_reponse_correspondante() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let ipc = MpvIpc::from_stream(client, tx);

        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_u64().unwrap();
            let resp = format!("{{\"error\":\"success\",\"data\":42,\"request_id\":{id}}}\n");
            w.write_all(resp.as_bytes()).await.unwrap();
        });

        let v = ipc.command(&[serde_json::json!("get_property"), serde_json::json!("volume")])
            .await
            .unwrap();
        assert_eq!(v, serde_json::json!(42));
    }

    #[tokio::test]
    async fn property_change_devient_event() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"media-title\",\"data\":\"FIP - Miles Davis\"}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":true}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":false}\n")
            .await
            .unwrap();
        w.write_all(b"{\"event\":\"property-change\",\"name\":\"playlist-pos\",\"data\":3}\n")
            .await
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), Event::Title("FIP - Miles Davis".into()));
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackIdle);
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(3));
    }

    #[tokio::test]
    async fn erreur_mpv_remonte_en_err() {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let ipc = MpvIpc::from_stream(client, tx);
        tokio::spawn(async move {
            let (r, mut w) = server.into_split();
            let mut lines = BufReader::new(r).lines();
            let line = lines.next_line().await.unwrap().unwrap();
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_u64().unwrap();
            let resp = format!("{{\"error\":\"invalid parameter\",\"request_id\":{id}}}\n");
            w.write_all(resp.as_bytes()).await.unwrap();
        });
        assert!(ipc.command(&[serde_json::json!("loadfile")]).await.is_err());
    }

    #[tokio::test]
    async fn metadata_icy_devient_un_evenement_de_titre() {
        // Capture réelle : forme du `property-change` que mpv émet pour la
        // propriété `metadata` sur un flux Icecast (SomaFM Groove Salad, le seul
        // des cinq flux mesurés à émettre un StreamTitle exploitable).
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);
        let (_r, mut w) = server.into_split();
        w.write_all(
            b"{\"event\":\"property-change\",\"name\":\"metadata\",\"data\":{\"icy-br\":\"128\",\"icy-title\":\"Mandrillus Sphynx - Bikwix\"}}\n",
        )
        .await
        .unwrap();
        assert_eq!(rx.recv().await.unwrap(), Event::IcyTitle("Mandrillus Sphynx - Bikwix".into()));
    }

    #[test]
    fn icy_title_ignore_le_vide_et_labsence() {
        // Cas mesurés : Radio Nova envoie un StreamTitle vide, FIP n'envoie
        // aucun en-tête ICY (pas d'icy-metaint du tout).
        assert_eq!(icy_title(&serde_json::json!({"icy-title": ""})), None);
        assert_eq!(icy_title(&serde_json::json!({"icy-title": "   "})), None);
        assert_eq!(icy_title(&serde_json::json!({"icy-br": "128"})), None);
        assert_eq!(icy_title(&serde_json::json!({})), None);
        // `metadata` vaut null tant qu'aucun fichier n'est chargé.
        assert_eq!(icy_title(&Value::Null), None);
        assert_eq!(icy_title(&serde_json::json!("pas un objet")), None);
        // Une valeur non textuelle ne doit pas paniquer.
        assert_eq!(icy_title(&serde_json::json!({"icy-title": 42})), None);
    }

    #[test]
    fn icy_title_tolere_la_casse_et_elague() {
        assert_eq!(
            icy_title(&serde_json::json!({"Icy-Title": "  Miles Davis - So What "})).as_deref(),
            Some("Miles Davis - So What")
        );
        assert_eq!(icy_title(&serde_json::json!({"ICY-TITLE": "x"})).as_deref(), Some("x"));
    }

    #[test]
    fn les_proprietes_utiles_sont_toutes_observees() {
        // Sans `observe_property`, mpv ne pousse jamais la propriété : la couche
        // ICY resterait muette sans qu'aucun test de `icy_title` ne s'en
        // aperçoive. `start` lançant un vrai processus mpv, c'est la liste
        // qu'elle parcourt qui est vérifiée ici.
        assert!(OBSERVEES.contains(&"metadata"), "sans elle, aucun titre ICY n'arrive jamais");
        assert!(OBSERVEES.contains(&"idle-active"), "sans elle, plus de relance apres coupure");
        assert!(OBSERVEES.contains(&"media-title"));
        assert!(OBSERVEES.contains(&"playlist-pos"));
    }

    #[test]
    fn variable_absente_donne_le_defaut_sans_bruit() {
        assert_eq!(audio_buffer_regle(None), AUDIO_BUFFER_DEFAUT);
        assert_eq!(readahead_regle(None), READAHEAD_DEFAUT);
    }

    #[test]
    fn une_valeur_valide_est_retenue() {
        assert_eq!(audio_buffer_regle(Some("1.5")), 1.5);
        assert_eq!(audio_buffer_regle(Some("  2  ")), 2.0);
        assert_eq!(readahead_regle(Some("30")), 30.0);
        // 0 est légitime : c'est la façon de revenir au comportement le plus
        // réactif, au prix de la robustesse.
        assert_eq!(audio_buffer_regle(Some("0")), 0.0);
    }

    #[test]
    fn une_valeur_invalide_retombe_sur_le_defaut() {
        for brut in ["", "abc", "-1", "1,5", "NaN", "inf"] {
            assert_eq!(audio_buffer_regle(Some(brut)), AUDIO_BUFFER_DEFAUT, "brut={brut:?}");
        }
        // Hors borne haute : mpv refuserait au-delà de 10 s pour le tampon de
        // sortie, et une avance de lecture démesurée coûte de la mémoire sans
        // bénéfice.
        assert_eq!(audio_buffer_regle(Some("42")), AUDIO_BUFFER_DEFAUT);
        assert_eq!(readahead_regle(Some("999")), READAHEAD_DEFAUT);
    }

    #[test]
    fn les_defauts_reproduisent_ceux_de_mpv() {
        // mpv 0.37 : --audio-buffer=0.2 et --demuxer-readahead-secs=1 (mesuré
        // par `mpv --list-options`). Ce module rend les deux réglables sans
        // changer le comportement par défaut : sans variable définie, mpv doit
        // se comporter exactement comme s'il était lancé sans ces options.
        // Toute dérive de ces valeurs est un changement de comportement audio
        // qui doit être voulu, pas un effet de bord — d'où ce test.
        assert_eq!(audio_buffer_regle(None), 0.2);
        assert_eq!(readahead_regle(None), 1.0);
    }

    #[test]
    fn les_arguments_portent_les_deux_tampons() {
        let args = mpv_args(std::path::Path::new("/run/rp/mpv.sock"), "/dev/sr0", 0.5, 10.0);
        assert!(args.contains(&"--audio-buffer=0.5".to_string()), "{args:?}");
        assert!(args.contains(&"--demuxer-readahead-secs=10".to_string()), "{args:?}");
        // Les arguments préexistants ne doivent pas avoir été perdus au passage.
        assert!(args.contains(&"--idle=yes".to_string()));
        assert!(args.contains(&"--no-video".to_string()));
        assert!(args.contains(&"--no-terminal".to_string()));
        assert!(args.contains(&"--input-ipc-server=/run/rp/mpv.sock".to_string()));
        assert!(args.contains(&"--cdda-device=/dev/sr0".to_string()));
    }
}
