use crate::player::Progression;
use crate::types::Event;
use anyhow::{bail, Context, Result};
use ritornello_proto::Morceau;
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
            // Vrai jusqu'à la première notification d'`idle-active`.
            //
            // `observe_property` renvoie aussitôt la valeur courante, et mpv est
            // lancé en démon **idle** : cette première valeur est donc toujours
            // `true`, et elle décrit un état de départ, pas un arrêt de lecture.
            // Le cœur, lui, lit `PlaybackIdle` comme la fin de ce qui jouait —
            // il pose `lecture = false` et notifie `Stop` à la Source.
            //
            // Mesuré à l'usage : l'événement attend dans le canal pendant que le
            // démarrage lance la première lecture, et il est traité juste après.
            // Sur un contenu fini (un fichier), rien ne le rattrape — plus de
            // « en écoute », rembobinage et avance grisés, position absente,
            // jusqu'à ce qu'un play/pause recharge tout depuis le début. Un flux
            // repassait par la relance et masquait le défaut.
            let mut premier_idle = true;
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
                        // Une même propriété, deux couches : l'en-tête ICY d'un
                        // flux, ou les tags d'un fichier. `file_tags` se tait
                        // dès qu'une clé ICY est présente, les deux branches
                        // sont donc exclusives — l'ordre ci-dessous n'est pas
                        // une priorité déguisée.
                        (Some("metadata"), data) => icy_title(data)
                            .map(Event::IcyTitle)
                            .or_else(|| file_tags(data).map(Event::FileTags)),
                        // Le chemin réellement ouvert par mpv, jamais déduit
                        // de l'identité opaque de la Source (voir `OBSERVEES`).
                        (Some("path"), Value::String(p)) => Some(Event::Path(p.clone())),
                        // La valeur initiale de l'observation est avalée (voir
                        // `premier_idle`) ; les suivantes suivent une lecture et
                        // sont de vrais arrêts, y compris la fin d'une liste.
                        (Some("idle-active"), Value::Bool(true)) => {
                            let initiale = std::mem::replace(&mut premier_idle, false);
                            if initiale { None } else { Some(Event::PlaybackIdle) }
                        }
                        (Some("idle-active"), Value::Bool(false)) => {
                            // Une entrée en lecture consomme aussi le droit
                            // d'avaler : si mpv annonce l'activité d'abord, le
                            // `true` qui suivra est un arrêt véritable.
                            premier_idle = false;
                            Some(Event::PlaybackActive)
                        }
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

/// Extrait les trois champs affichables des **tags du fichier joué**, depuis
/// cette même propriété `metadata`. Fonction pure, testable sur une capture
/// réelle.
///
/// FFmpeg **normalise** les clés : ID3 (mp3), Vorbis comments (flac, ogg,
/// opus), atomes iTunes (m4a) et RIFF INFO (wav) remontent tous sous
/// `title` / `artist` / `album`, ce qui a été vérifié format par format. Une
/// seule grammaire suffit donc, et elle couvre toute la bibliothèque.
///
/// Deux précautions, l'une et l'autre nées d'une mesure :
///
/// - on **pioche trois clés nommées** au lieu d'absorber l'objet : un m4a y
///   fait aussi remonter `major_brand`, `handler_name`, `vendor_id` et
///   `compatible_brands`, qui n'ont rien à faire dans un affichage ;
/// - la présence d'une clé `icy-*` **signe un flux** et rend `None`. Certaines
///   stations renseignent un `title` valant leur propre nom à côté d'un
///   `icy-title` qui porte le vrai morceau : préférer le premier serait une
///   régression pour la radio, silencieuse et difficile à attribuer.
pub fn file_tags(data: &Value) -> Option<Morceau> {
    let map = data.as_object()?;
    if map.keys().any(|cle| cle.to_ascii_lowercase().starts_with("icy-")) {
        return None;
    }
    let champ = |nom: &str| {
        map.iter()
            .find(|(cle, _)| cle.eq_ignore_ascii_case(nom))
            .and_then(|(_, valeur)| valeur.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let morceau = Morceau {
        artist: champ("artist"),
        title: champ("title"),
        album: champ("album"),
        duration_s: None,
        origin: Some(crate::metadata::ORIGINE_TAGS.to_string()),
        cover_href: None,
        cover_origin: None,
    };
    (!morceau.est_vide()).then_some(morceau)
}

/// Extrait la pochette embarquée du fichier joué, dans un fichier temporaire.
///
/// **Strictement bloquante** (lecture de fichier via `lofty`, potentiellement
/// sur un partage réseau) : à appeler uniquement sous `Sante::borne`, jamais
/// directement depuis une tâche asynchrone — voir `Core::handle_path` et
/// `sante.rs` pour la raison.
///
/// Un fichier plutôt que des octets en mémoire : cela garde **une seule
/// nature** de pochette locale côté cache, qui ne charge alors rien en RAM.
///
/// Tenté uniquement sur un chemin **sans schéma** : un flux n'a pas de tag, et
/// `lofty` n'a rien à ouvrir sur une URL.
///
/// Nommé d'après le **contenu de l'image**, pas d'après la piste (voir
/// `cover::cle_contenu`) : les pistes d'un même album à pochette unique
/// écrivent donc un seul fichier et publient un seul `href`, que
/// `relais_afficheur` reconnaît alors comme déjà poussé — plus de décodage,
/// plus de trame, plus de retéléchargement navigateur après la première.
/// Reste une lecture `lofty` par piste, incontournable : il faut les octets
/// pour les hacher.
///
/// **N'écrit que si le fichier est absent**, et c'est le nommage par contenu
/// qui rend ce raccourci sûr : un fichier déjà là sous ce nom porte, par
/// construction, l'image qu'on s'apprêtait à y mettre. Ce n'est pas qu'une
/// économie — réécrire aurait tronqué, le temps de l'écriture, le fichier
/// que la route HTTP est peut-être en train de servir pour la piste
/// précédente, qui porte désormais le même nom.
///
/// La fraîcheur ne repose donc plus sur une réécriture systématique mais sur
/// l'adressage par contenu. Ce que ce nommage ne couvre pas, en revanche,
/// c'est un fichier **tronqué** laissé par une exécution tuée en pleine
/// écriture : son nom annoncerait une image que son contenu ne porte pas, et
/// l'écriture conditionnelle l'adopterait. C'est `cover::purge_temporaires`,
/// au démarrage, qui ferme ce cas — la purge n'est plus seulement une borne
/// à l'accumulation, elle est devenue nécessaire à la correction.
pub fn pochette_embarquee(chemin: &str) -> Option<ritornello_proto::CoverRef> {
    if chemin.contains("://") {
        return None;
    }
    let fichier = lofty::probe::Probe::open(chemin).ok()?.read().ok()?;
    let image = lofty::file::TaggedFileExt::primary_tag(&fichier)
        .or_else(|| lofty::file::TaggedFileExt::first_tag(&fichier))?
        .pictures()
        .first()?
        .clone();
    let extension = match image.mime_type() {
        Some(m) if m.as_str().contains("png") => "png",
        Some(m) if m.as_str().contains("webp") => "webp",
        _ => "jpg",
    };
    let mut cible = std::env::temp_dir();
    cible.push(format!(
        "{}{}.{extension}",
        crate::cover::PREFIXE_TEMPORAIRE,
        crate::cover::cle_contenu(image.data())
    ));
    if !cible.exists() {
        std::fs::write(&cible, image.data()).ok()?;
    }
    Some(ritornello_proto::CoverRef::Path { path: cible.to_string_lossy().into_owned() })
}

pub struct MpvPlayer {
    ipc: Arc<MpvIpc>,
}

/// Ramène une réponse de `get_property` à un nombre utilisable.
///
/// Trois façons pour mpv de dire « je ne sais pas », toutes ramenées à
/// `None` : l'erreur (`property unavailable` sur un flux sans durée), le
/// `null`, et la valeur négative que mpv produit brièvement au démarrage d'un
/// fichier — mesuré à `-0.02`, et publier cela ferait reculer la barre.
fn nombre_ou_none(res: Result<Value>) -> Option<f64> {
    res.ok().and_then(|v| v.as_f64()).filter(|n| *n >= 0.0)
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
/// pour une radio sans plugin `metadata` dédié. `path` est la seule façon dont
/// le cœur apprend quel fichier joue : il a fait un principe de ne **jamais**
/// interpréter l'identité opaque produite par la Source pour en tirer un
/// chemin — c'est mpv, qui a réellement ouvert le fichier, qui le dit.
const OBSERVEES: [&str; 6] =
    ["media-title", "metadata", "idle-active", "playlist-pos", "chapter", "path"];

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
    /// `loadlist` et non `loadfile` : la liste est dépliée **avant** que la
    /// commande ne réponde (sa réponse porte même `num_entries`), si bien qu'un
    /// `playlist-pos` envoyé juste après tombe dans les bornes.
    ///
    /// Avec `loadfile`, mesuré sur mpv 0.37 : `playlist-count` vaut d'abord 1,
    /// la position 0, puis viennent un `end-file` et un `start-file` avant que
    /// le compte ne passe à 3. Le `playlist-pos` demandé arrivait donc hors
    /// bornes, et le dépliage rejouait la première piste.
    async fn load_list(&self, uri: &str) -> Result<()> {
        self.ipc.command(&[json!("loadlist"), json!(uri), json!("replace")]).await?;
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
    async fn progression(&self) -> Result<Progression> {
        // Deux allers-retours par seconde sur une socket Unix locale : le coût
        // est nul devant l'intervalle. Un sondage plutôt qu'un
        // `observe_property` parce que mpv ne cadence pas ses notifications de
        // `time-pos` — il en émettrait plusieurs par seconde pour une
        // information publiée une fois par seconde.
        //
        // Réserve non levée sur un `cdda://` ouvert en disque entier : mpv
        // expose alors ses pistes comme des chapitres (voir plus haut, à
        // propos de l'avance de piste). Que vaut `time-pos` dans ce cas —
        // relatif au disque ou à la piste ? Ce n'est pas mesuré sur le
        // matériel, seulement noté dans un document de conception archivé. Si
        // la réponse est « relatif au disque », cette valeur doit retrancher
        // le début du chapitre courant, et `duration_s` refléter celle du
        // chapitre plutôt que celle du disque entier.
        let position = self.ipc.command(&[json!("get_property"), json!("time-pos")]).await;
        let duree = self.ipc.command(&[json!("get_property"), json!("duration")]).await;
        Ok(Progression { position_s: nombre_ou_none(position), duration_s: nombre_ou_none(duree) })
    }

    async fn seek_relative(&self, delta_s: i64) -> Result<()> {
        self.ipc
            .command(&[json!("seek"), json!(delta_s), json!("relative")])
            .await
            .map(|_| ())
    }

    async fn seek_absolute(&self, position_s: u32) -> Result<()> {
        self.ipc
            .command(&[json!("seek"), json!(position_s), json!("absolute")])
            .await
            .map(|_| ())
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
        // Le `idle-active: true` envoyé ci-dessus est la **première** valeur
        // observée : elle décrit l'état de départ du démon idle, pas un arrêt,
        // et elle est donc avalée (voir
        // `le_premier_idle_observe_n_est_pas_un_arret`). Ce test l'attendait
        // autrefois comme un événement — il encodait le défaut.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(3));
    }

    #[tokio::test]
    async fn le_premier_idle_observe_n_est_pas_un_arret() {
        // mpv est lancé en démon idle, et `observe_property` renvoie aussitôt la
        // valeur courante : `idle-active = true` arrive donc avant toute
        // lecture. C'est un état de départ, pas un arrêt — mais le cœur traite
        // `PlaybackIdle` comme la fin de ce qui jouait (`lecture = false`, et
        // `Stop` notifié à la Source).
        //
        // Défaut mesuré à l'usage : cet événement attend dans le canal pendant
        // que le démarrage lance la première lecture, et il est traité juste
        // après. Sur un contenu **fini** — un fichier — rien ne le rattrape :
        // pas de « en écoute », rembobinage et avance grisés, position absente,
        // jusqu'à ce qu'un play/pause recharge tout depuis le début. Un flux,
        // lui, repassait par la branche de relance (`expecting_stream`) et
        // rejouait tout seul, ce qui masquait le défaut côté radio.
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        // Dans l'ordre : la valeur initiale de l'observation, un vrai
        // chargement, puis un vrai arrêt en fin de liste.
        for data in ["true", "false", "true"] {
            w.write_all(
                format!("{{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":{data}}}\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        }

        // Le premier `true` est avalé : le premier événement reçu est l'entrée
        // en lecture.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        // Le second, lui, suit une lecture : c'est un arrêt véritable, et il
        // doit passer — sans quoi la fin d'une liste ne s'afficherait plus.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackIdle);
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
    fn les_tags_dun_fichier_local_donnent_les_trois_champs() {
        // Charge relevée au banc sur un mp3 (ID3). FFmpeg normalise les clés :
        // flac, ogg, opus, m4a et wav ont été vérifiés et remontent sous les
        // mêmes noms, donc une seule grammaire à connaître.
        let data = serde_json::json!({
            "title": "So What", "artist": "Miles Davis",
            "album": "Kind of Blue", "encoder": "Lavf60.16.100"
        });
        let m = file_tags(&data).unwrap();
        assert_eq!(m.title.as_deref(), Some("So What"));
        assert_eq!(m.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(m.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(m.origin.as_deref(), Some("tags"));
    }

    #[test]
    fn les_cles_de_conteneur_m4a_sont_ignorees() {
        // Relevé au banc : un m4a fait aussi remonter des clés de conteneur.
        // On pioche trois clés nommées, on n'absorbe jamais l'objet.
        let data = serde_json::json!({
            "title": "So What", "major_brand": "M4A ", "handler_name": "SoundHandler",
            "vendor_id": "[0][0][0][0]", "compatible_brands": "M4A mp42isom"
        });
        let m = file_tags(&data).unwrap();
        assert_eq!(m.title.as_deref(), Some("So What"));
        assert_eq!(m.artist, None);
        assert_eq!(m.album, None);
    }

    #[test]
    fn une_charge_icy_ne_produit_aucun_tag() {
        // La garde qui protège la radio : certaines stations renseignent un
        // `title` valant le NOM DE LA STATION à côté d'un `icy-title` qui
        // porte le vrai morceau. Préférer le premier serait une régression
        // silencieuse — le titre du morceau remplacé par le nom de la station.
        let data = serde_json::json!({
            "icy-br": "128", "icy-title": "Mandrillus Sphynx - Bikwix", "title": "OUI FM"
        });
        assert!(file_tags(&data).is_none());
        assert_eq!(icy_title(&data).as_deref(), Some("Mandrillus Sphynx - Bikwix"));
    }

    #[test]
    fn une_charge_sans_rien_de_lisible_ne_produit_aucun_tag() {
        // Un enrichissement vide compterait comme une réponse et masquerait
        // l'ICY : il ne doit pas exister.
        assert!(file_tags(&serde_json::json!({"encoder": "Lavf60.16.100"})).is_none());
        assert!(file_tags(&serde_json::json!({"title": "   "})).is_none());
        assert!(file_tags(&serde_json::json!({})).is_none());
        assert!(file_tags(&Value::Null).is_none());
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
    fn la_propriete_path_est_observee() {
        // Sans elle, le coeur ne sait jamais quel fichier mpv joue, et la
        // pochette embarquee n'est jamais lue. Le coeur ne lit pas le chemin
        // dans l'identite : il a fait un principe de ne jamais l'interpreter.
        assert!(OBSERVEES.contains(&"path"), "sans elle, aucune pochette embarquee");
    }

    #[test]
    fn un_flux_ne_declenche_aucune_extraction() {
        // Tente uniquement sur un chemin sans schema.
        assert!(pochette_embarquee("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        assert!(pochette_embarquee("http://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(pochette_embarquee("/n/existe/pas.flac").is_none());
    }

    /// Fabrique un mp3 réel avec une pochette embarquée, via ffmpeg, ou rend
    /// `None` s'il est absent.
    ///
    /// Comme dans `ritornello-plugin-files::duree` : pas de binaire versionné
    /// dans le dépôt, et le test se saute plutôt que d'échouer là où ffmpeg
    /// manque — c'est un outil de développement, pas une dépendance du cœur.
    ///
    /// `source_image` est un filtre `lavfi`, donc l'image embarquée, donc —
    /// depuis que le temporaire est nommé d'après son contenu — **le nom du
    /// fichier temporaire lui-même**. Deux tests parallèles qui embarquent la
    /// même image viseraient le même chemin dans le `temp_dir()` partagé :
    /// chacun doit donc demander une image à lui.
    fn mp3_avec_pochette_de(dir: &Path, source_image: &str) -> Option<std::path::PathBuf> {
        let image = dir.join("cover.jpg");
        let sortie = dir.join("avec_pochette.mp3");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", source_image])
            .args(["-frames:v", "1"])
            .arg(&image)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            && std::process::Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg("sine=frequency=440:duration=1")
                .arg("-i")
                .arg(&image)
                .args(["-map", "0:a", "-map", "1:v", "-c:a", "libmp3lame", "-c:v", "copy"])
                .args(["-id3v2_version", "3"])
                .args(["-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"])
                .arg(&sortie)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        ok.then_some(sortie)
    }

    fn mp3_avec_pochette(dir: &Path) -> Option<std::path::PathBuf> {
        mp3_avec_pochette_de(dir, "color=c=red:s=16x16:d=1")
    }

    #[test]
    fn la_pochette_embarquee_dun_fichier_local_est_extraite_dans_un_fichier() {
        let dir = tempfile::tempdir().unwrap();
        let Some(f) = mp3_avec_pochette(dir.path()) else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        let r = pochette_embarquee(f.to_str().unwrap()).expect("une pochette embarquee attendue");
        let ritornello_proto::CoverRef::Path { path } = r else {
            panic!("une pochette locale doit rendre un CoverRef::Path");
        };
        // Un vrai JPEG a été écrit sur disque, pas de bidon ni des octets en
        // mémoire : c'est ce qui garde une seule nature de pochette locale
        // côté cache.
        let octets = std::fs::read(&path).expect("le fichier temporaire doit exister");
        assert!(octets.starts_with(&[0xFF, 0xD8, 0xFF]), "en-tete JPEG attendu, lu {octets:?}");
        assert!(path.ends_with(".jpg"), "{path}");

        // Rejouer la même piste doit retomber sur le même fichier temporaire :
        // c'est ce qui évite d'écrire deux fois la même image.
        let r2 = pochette_embarquee(f.to_str().unwrap()).unwrap();
        assert_eq!(r2, ritornello_proto::CoverRef::Path { path });
    }

    #[test]
    fn deux_pistes_du_meme_album_partagent_un_seul_fichier_temporaire() {
        let dir = tempfile::tempdir().unwrap();
        // Une image propre à ce test : voir `mp3_avec_pochette_de`.
        let Some(piste1) = mp3_avec_pochette_de(dir.path(), "color=c=blue:s=24x24:d=1") else {
            eprintln!("ffmpeg absent : test saute");
            return;
        };
        // Deux fichiers de piste distincts portant la même pochette : le cas
        // courant d'un album, et celui que le nommage par chemin de piste
        // faisait payer quinze fois pour une seule image.
        let piste2 = dir.path().join("piste_2.mp3");
        std::fs::copy(&piste1, &piste2).unwrap();

        let r1 = pochette_embarquee(piste1.to_str().unwrap()).expect("une pochette attendue");
        let r2 = pochette_embarquee(piste2.to_str().unwrap()).expect("une pochette attendue");
        assert_eq!(r1, r2, "deux pistes a pochette identique doivent rendre le meme fichier");
        let ritornello_proto::CoverRef::Path { path } = r1 else {
            panic!("une pochette locale doit rendre un CoverRef::Path");
        };

        // Rien n'est reecrit quand le nom est deja pris : la sentinelle
        // survit. Sans le `if !cible.exists()`, la route HTTP pourrait servir
        // ce fichier tronque pendant sa reecriture pour la piste suivante.
        std::fs::write(&path, b"sentinelle").unwrap();
        let r3 = pochette_embarquee(piste2.to_str().unwrap()).unwrap();
        assert_eq!(r3, ritornello_proto::CoverRef::Path { path: path.clone() });
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"sentinelle".to_vec(),
            "un fichier deja present ne doit pas etre reecrit"
        );

        let _ = std::fs::remove_file(&path);
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

    /// mpv répond `null` sur `time-pos` quand rien n'est chargé, et une
    /// **erreur** quand la propriété n'est pas disponible. Les deux disent la
    /// même chose — « je ne sais pas » — et aucune n'est une panne à faire
    /// remonter : une position inconnue est un cas normal, pas un incident.
    #[test]
    fn une_valeur_absente_ou_nulle_devient_none() {
        assert_eq!(nombre_ou_none(Ok(serde_json::json!(87.4))), Some(87.4));
        assert_eq!(nombre_ou_none(Ok(serde_json::Value::Null)), None);
        assert_eq!(nombre_ou_none(Err(anyhow::anyhow!("property unavailable"))), None);
    }

    /// Une position négative n'existe pas, et mpv en produit brièvement au
    /// démarrage d'un fichier (mesuré : `-0.02`). La publier ferait afficher
    /// une barre qui recule.
    #[test]
    fn une_valeur_negative_devient_none() {
        assert_eq!(nombre_ou_none(Ok(serde_json::json!(-0.02))), None);
    }
}
