use crate::player::Progress;
use crate::types::Event;
use anyhow::{bail, Context, Result};
use ritornello_proto::Track;
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
            // True until the first `idle-active` notification.
            //
            // `observe_property` immediately returns the current value, and
            // mpv is launched as an **idle** daemon: this first value is
            // therefore always `true`, and it describes a starting state,
            // not a playback stop. The core, for its part, reads
            // `PlaybackIdle` as the end of what was playing — it sets
            // `playback = false` and notifies `Stop` to the Source.
            //
            // Measured in practice: the event waits in the channel while
            // startup launches the first playback, and it is processed
            // right after. On finite content (a file), nothing catches it
            // — no more "listening", rewind and forward greyed out,
            // position absent, until a play/pause reloads everything from
            // the start. A stream would go back through the restart and
            // mask the defect.
            let mut first_idle = true;
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
                        // One property, two layers: the ICY header of a
                        // stream, or the tags of a file. `file_tags` stays
                        // silent as soon as an ICY key is present, so the
                        // two branches are exclusive — the order below is
                        // not a disguised priority.
                        (Some("metadata"), data) => icy_title(data)
                            .map(Event::IcyTitle)
                            .or_else(|| file_tags(data).map(|m| Event::FileTags(Box::new(m)))),
                        // The path mpv actually opened, never inferred from
                        // the Source's opaque identity (see `OBSERVED`).
                        (Some("path"), Value::String(p)) => Some(Event::Path(p.clone())),
                        // The observation's initial value is swallowed (see
                        // `first_idle`); the following ones follow a
                        // playback and are true stops, including the end
                        // of a playlist.
                        (Some("idle-active"), Value::Bool(true)) => {
                            let initial = std::mem::replace(&mut first_idle, false);
                            if initial { None } else { Some(Event::PlaybackIdle) }
                        }
                        (Some("idle-active"), Value::Bool(false)) => {
                            // Entering playback also consumes the right to
                            // swallow: if mpv announces activity first, the
                            // `true` that follows is a genuine stop.
                            first_idle = false;
                            Some(Event::PlaybackActive)
                        }
                        // Two properties for the same fact, track advance:
                        // mpv exposes a CD's tracks either as playlist
                        // entries or as chapters, depending on how the disc
                        // was opened (whole `cdda://` or `cdda://<track>`).
                        // Only one of the two speaks at a time, then, and
                        // the core relays the same thing in both cases — it
                        // is the Source that knows what "track n" means. A
                        // negative index (mpv says `-1` when there is no
                        // chapter) is passed through as-is and discarded by
                        // the Source.
                        (Some("playlist-pos") | Some("chapter"), Value::Number(n)) => {
                            n.as_i64().map(Event::TrackChanged)
                        }
                        _ => None,
                    };
                    if let Some(ev) = ev {
                        // `mpsc` with no loss: a full channel means
                        // backpressure on this pump (the socket's readback
                        // waits), never a dropped event. A vanished
                        // receiver means the core loop is done, nobody
                        // left to serve.
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

/// Extracts the title the stream announces from the content of mpv's
/// `metadata` property. Pure function, testable on a real capture.
///
/// The key is looked up **case-insensitively**: mpv copies field names
/// exactly as the station sends them, and the ICY header shows up,
/// depending on the server, as `icy-title`, `Icy-Title` or `ICY-TITLE`.
///
/// An empty or blank value gives `None`, hence no event: several stations
/// measured emit an empty `StreamTitle` between two tracks (and OUI FM puts
/// filler text there). Clearing the display on every gap would make the
/// line flicker, while a track change already resets the slate on the core
/// side.
pub fn icy_title(data: &Value) -> Option<String> {
    let map = data.as_object()?;
    let raw = map
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("icy-title"))
        .and_then(|(_, value)| value.as_str())?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Extracts the three displayable fields from the **tags of the played
/// file**, from that same `metadata` property. Pure function, testable on a
/// real capture.
///
/// FFmpeg **normalizes** the keys: ID3 (mp3), Vorbis comments (flac, ogg,
/// opus), iTunes atoms (m4a) and RIFF INFO (wav) all surface under
/// `title` / `artist` / `album`, which was verified format by format. A
/// single grammar is therefore enough, and it covers the whole library.
///
/// Two precautions, both born from a measurement:
///
/// - only **three named keys are picked out** instead of absorbing the
///   object: an m4a also surfaces `major_brand`, `handler_name`,
///   `vendor_id` and `compatible_brands`, which have no place in a
///   display;
/// - the presence of an `icy-*` key **marks a stream** and returns `None`.
///   Some stations fill in a `title` equal to their own name alongside an
///   `icy-title` that carries the real track: preferring the former would
///   be a silent regression for the radio.
pub fn file_tags(data: &Value) -> Option<Track> {
    let map = data.as_object()?;
    if map.keys().any(|key| key.to_ascii_lowercase().starts_with("icy-")) {
        return None;
    }
    let field = |name: &str| {
        map.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let track = Track {
        artist: field("artist"),
        title: field("title"),
        album: field("album"),
        duration_s: None,
        // FFmpeg normalizes the year under `date`, whatever the file
        // format — as it already does for the three fields above. Its
        // value ranges from `"1959"` to `"1959-08-17"` depending on what
        // the tag carries, hence the pass through `valid_year`.
        year: field("date").as_deref().and_then(ritornello_proto::valid_year),
        // A local file carries no platform link: only streams announce
        // those.
        links: Vec::new(),
        origin: Some(crate::metadata::ORIGIN_TAGS.to_string()),
        cover_href: None,
        cover_origin: None,
        // Left empty here: it is `Metadata::text_block` that attributes
        // these fields to the tags, at composition time. Filling it in
        // here too would give two truths to keep in sync.
        provenance: Default::default(),
    };
    (!track.is_empty()).then_some(track)
}

/// Probes the played file for an embedded cover, without writing anything.
///
/// **Strictly blocking** (file playback via `lofty`, potentially on a
/// network share): call only under `Health::bounded`, never directly from
/// an async task — see `Core::handle_path` and `health.rs` for why.
///
/// **Symmetrical with a Source's `folder.jpg`, and that symmetry is the whole
/// rework.** A local cover — wherever it lives — costs the core only a path:
/// `CoverPayload::File` re-reads a `folder.jpg` from its path on every
/// request, and `CoverPayload::Embedded` now does the same for a picture
/// inside the audio file, re-opening it through `read_embedded_bounded`
/// rather than keeping a copy anywhere. Nothing here loads the picture's
/// bytes into a `CoverPayload`, and nothing here touches the disk to produce
/// one: this function's only output is `cover::CoverSource::Embedded { audio,
/// content }`, the audio file's own path plus a fingerprint of the picture's
/// bytes.
///
/// Only attempted on a path **with no scheme**: a stream has no tag, and
/// `lofty` has nothing to open on a URL.
///
/// `content` (see `cover::content_key`) is what a fifteen-track album needs
/// to collapse onto a single cache entry: two tracks sharing one embedded
/// picture yield two different `audio` paths but the identical `content`,
/// and `cover::key` hashes only the latter — see its doc. Computing it still
/// costs one `lofty` read per track, that part is irreducible: the bytes must
/// be in hand to be hashed, whether or not they end up written anywhere.
///
/// **This function used to write a temp file here, named after that same
/// `content`, and no longer does.** The write bought nothing this task's
/// symmetry does not already give for free — the local-file path never
/// needed to copy a `folder.jpg` into `/tmp` before serving it, and once the
/// core stopped needing a filesystem path *distinct from the audio file* to
/// name a `CoverPayload`, there was nothing left for the write to buy. What
/// it used to cost is gone with it, and so has the machinery that only
/// existed to bound that cost: a write per newly-seen track, the startup
/// sweep that a killed-mid-write truncation made necessary for correctness,
/// and the accumulation — one file per distinct embedded picture ever
/// played — on a `tmpfs` that only a reboot clears. None of `cover.rs` still
/// names or sweeps such a file; there is nothing left to name.
pub fn embedded_cover(path: &str) -> Option<crate::cover::CoverSource> {
    if path.contains("://") {
        return None;
    }
    let file = lofty::probe::Probe::open(path).ok()?.read().ok()?;
    let image = lofty::file::TaggedFileExt::primary_tag(&file)
        .or_else(|| lofty::file::TaggedFileExt::first_tag(&file))?
        .pictures()
        .first()?;
    Some(crate::cover::CoverSource::Embedded {
        audio: std::path::PathBuf::from(path),
        content: crate::cover::content_key(image.data()),
    })
}

pub struct MpvPlayer {
    ipc: Arc<MpvIpc>,
}

/// Brings a `get_property` response down to a usable number.
///
/// Three ways for mpv to say "I don't know", all brought down to `None`:
/// the error (`property unavailable` on a stream with no duration), the
/// `null`, and the negative value mpv briefly produces at a file's startup
/// — measured at `-0.02`, and publishing that would make the bar go
/// backwards.
fn number_or_none(res: Result<Value>) -> Option<f64> {
    res.ok().and_then(|v| v.as_f64()).filter(|n| *n >= 0.0)
}

/// Audio output buffer, in seconds. **We reuse mpv's default**, so this
/// module changes nothing about behavior as long as the variable is not
/// set: the cause of the observed micro-glitches is not established, and
/// widening it by default would have masked the diagnosis rather than
/// making it. The dial exists because the right value depends on the
/// machine — on a Pi 2, a load spike can make an ALSA write deadline be
/// missed, which is heard as a micro-glitch, and raising it to 0.5 s is
/// then the first thing to try. The cost is a matching latency on picking
/// up volume or mute changes, imperceptible for radio.
pub const AUDIO_BUFFER_DEFAULT: f64 = 0.2;

/// Upper bound mpv imposes on `--audio-buffer`.
const AUDIO_BUFFER_MAX: f64 = 10.0;

/// Playback read-ahead, in seconds. **We reuse mpv's default**, for the
/// same reason as the output buffer: change nothing without having
/// measured. One second is thin for an internet stream, though — the
/// slightest network jitter clears the read-ahead and mpv pauses playback
/// while it refills — so this is the dial to turn first on a flaky link.
/// Ten seconds of 128 kbit/s MP3 weighs about 160 KB, negligible even on 1
/// GB of RAM.
pub const READAHEAD_DEFAULT: f64 = 1.0;

/// Upper bound kept here: beyond it, the buffer costs memory with no
/// audible benefit, and delays picking up a station change.
const READAHEAD_MAX: f64 = 120.0;

/// Reads a duration supplied by the environment. Variable absent: the
/// default, silently. Value unreadable, negative or out of bounds: the
/// default **with** a warning, rather than a startup failure — a silent
/// device because a variable was mistyped would be a worse outcome than a
/// default setting.
fn duration_setting(raw: Option<&str>, default: f64, max: f64, what: &str) -> f64 {
    let Some(raw) = raw else { return default };
    match raw.trim().parse::<f64>() {
        Ok(v) if v.is_finite() && (0.0..=max).contains(&v) => v,
        Ok(v) => {
            tracing::warn!("{what}={v} out of bounds (0..={max}), keeping {default}");
            default
        }
        Err(e) => {
            tracing::warn!("{what}={raw:?} unreadable ({e}), keeping {default}");
            default
        }
    }
}

/// Output buffer to keep, based on `RITORNELLO_AUDIO_BUFFER` if it is set.
pub fn audio_buffer_setting(raw: Option<&str>) -> f64 {
    duration_setting(raw, AUDIO_BUFFER_DEFAULT, AUDIO_BUFFER_MAX, "RITORNELLO_AUDIO_BUFFER")
}

/// Playback read-ahead to keep, based on `RITORNELLO_NETWORK_READAHEAD`.
pub fn readahead_setting(raw: Option<&str>) -> f64 {
    duration_setting(raw, READAHEAD_DEFAULT, READAHEAD_MAX, "RITORNELLO_NETWORK_READAHEAD")
}

/// mpv launch arguments. Pure function, separate from `start` to be
/// testable without spawning a process.
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

/// Properties the core asks mpv to push. `metadata` carries the ICY header
/// received from the station (key `icy-title`), the only title source
/// available for a radio with no dedicated `metadata` plugin. `path` is the
/// only way the core learns which file is playing: it made a principle of
/// **never** interpreting the opaque identity produced by the Source to
/// derive a path from it — it is mpv, which actually opened the file, that
/// says so.
const OBSERVED: [&str; 6] =
    ["media-title", "metadata", "idle-active", "playlist-pos", "chapter", "path"];

/// Launches mpv as an idle daemon and connects to it. The Child is handed
/// back to the caller: if it dies, main exits and systemd restarts the
/// whole service.
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
    for property in OBSERVED {
        ipc.observe(property).await?;
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
    /// `loadlist` and not `loadfile`: the list is unfolded **before** the
    /// command answers (its response even carries `num_entries`), so a
    /// `playlist-pos` sent right after falls within bounds.
    ///
    /// With `loadfile`, measured on mpv 0.37: `playlist-count` is first 1,
    /// position 0, then an `end-file` and a `start-file` come before the
    /// count reaches 3. The requested `playlist-pos` therefore arrived out
    /// of bounds, and the unfolding replayed the first track.
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
    async fn progress(&self) -> Result<Progress> {
        // Two round trips per second on a local Unix socket: the cost is
        // nil next to the interval. A poll rather than an
        // `observe_property` because mpv does not pace its `time-pos`
        // notifications — it would emit several per second for information
        // published once per second.
        //
        // Open question, not resolved: on a `cdda://` opened as a whole
        // disc, mpv exposes its tracks as chapters (see above, about track
        // advance). What does `time-pos` mean in that case — relative to
        // the disc or to the track? This is not measured on real hardware,
        // only noted in an archived design document. If the answer is
        // "relative to the disc", this value must subtract the start of
        // the current chapter, and `duration_s` should reflect the
        // chapter's rather than the whole disc's.
        let position = self.ipc.command(&[json!("get_property"), json!("time-pos")]).await;
        let duration = self.ipc.command(&[json!("get_property"), json!("duration")]).await;
        Ok(Progress { position_s: number_or_none(position), duration_s: number_or_none(duration) })
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
pub(crate) mod tests {
    use super::*;
    use crate::types::Event;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn command_receives_the_matching_response() {
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
    async fn property_change_becomes_an_event() {
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
        // The `idle-active: true` sent above is the **first** observed
        // value: it describes the idle daemon's starting state, not a
        // stop, and is therefore swallowed (see
        // `the_first_observed_idle_is_not_a_stop`). This test used to
        // expect it as an event — it encoded the defect.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        assert_eq!(rx.recv().await.unwrap(), Event::TrackChanged(3));
    }

    #[tokio::test]
    async fn the_first_observed_idle_is_not_a_stop() {
        // mpv is launched as an idle daemon, and `observe_property`
        // immediately returns the current value: `idle-active = true`
        // therefore arrives before any playback. It is a starting state,
        // not a stop — but the core treats `PlaybackIdle` as the end of
        // what was playing (`playback = false`, and `Stop` notified to the
        // Source).
        //
        // Defect measured in practice: this event waits in the channel
        // while startup launches the first playback, and it is processed
        // right after. On **finite** content — a file — nothing catches
        // it: no "listening", rewind and forward greyed out, position
        // absent, until a play/pause reloads everything from the start. A
        // stream, on the other hand, would go back through the restart
        // branch (`expecting_stream`) and replay on its own, which masked
        // the defect on the radio side.
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let _ipc = MpvIpc::from_stream(client, tx);

        let (_r, mut w) = server.into_split();
        // In order: the observation's initial value, a real load, then a
        // real stop at the end of a playlist.
        for data in ["true", "false", "true"] {
            w.write_all(
                format!("{{\"event\":\"property-change\",\"name\":\"idle-active\",\"data\":{data}}}\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        }

        // The first `true` is swallowed: the first event received is
        // entering playback.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackActive);
        // The second one, though, follows a playback: it is a genuine
        // stop, and it must go through — otherwise the end of a playlist
        // would no longer display.
        assert_eq!(rx.recv().await.unwrap(), Event::PlaybackIdle);
    }

    #[tokio::test]
    async fn mpv_error_bubbles_up_as_err() {
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
    async fn icy_metadata_becomes_a_title_event() {
        // Real capture: shape of the `property-change` mpv emits for the
        // `metadata` property on an Icecast stream (SomaFM Groove Salad,
        // the only one of five streams measured to emit a usable
        // StreamTitle).
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
    fn a_local_files_tags_give_the_four_fields() {
        // Payload recorded on the bench for an mp3 (ID3). FFmpeg normalizes
        // the keys: flac, ogg, opus, m4a and wav were checked and surface
        // under the same names, so a single grammar to know. `date` is one
        // of them and carries the long form here: without this assertion,
        // reading `year` from the wrong key (`year`, which FFmpeg does not
        // emit) would have gone unnoticed.
        let data = serde_json::json!({
            "title": "So What", "artist": "Miles Davis",
            "album": "Kind of Blue", "date": "1959-08-17", "encoder": "Lavf60.16.100"
        });
        let m = file_tags(&data).unwrap();
        assert_eq!(m.title.as_deref(), Some("So What"));
        assert_eq!(m.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(m.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(m.year, Some(1959));
        assert_eq!(m.origin.as_deref(), Some("tags"));
    }

    #[test]
    fn m4a_container_keys_are_ignored() {
        // Recorded on the bench: an m4a also surfaces container keys. Four
        // named keys are picked out, the object is never absorbed.
        let data = serde_json::json!({
            "title": "So What", "major_brand": "M4A ", "handler_name": "SoundHandler",
            "vendor_id": "[0][0][0][0]", "compatible_brands": "M4A mp42isom"
        });
        let m = file_tags(&data).unwrap();
        assert_eq!(m.title.as_deref(), Some("So What"));
        assert_eq!(m.artist, None);
        assert_eq!(m.album, None);
        assert_eq!(m.year, None);
    }

    #[test]
    fn an_icy_payload_produces_no_tag() {
        // The guard that protects the radio: some stations fill in a
        // `title` equal to the STATION NAME alongside an `icy-title` that
        // carries the real track. Preferring the former would be a silent
        // regression — the track's title replaced by the station's name.
        let data = serde_json::json!({
            "icy-br": "128", "icy-title": "Mandrillus Sphynx - Bikwix", "title": "OUI FM"
        });
        assert!(file_tags(&data).is_none());
        assert_eq!(icy_title(&data).as_deref(), Some("Mandrillus Sphynx - Bikwix"));
    }

    #[test]
    fn a_payload_with_nothing_readable_produces_no_tag() {
        // An empty enrichment would count as a response and would mask the
        // ICY: it must not exist.
        assert!(file_tags(&serde_json::json!({"encoder": "Lavf60.16.100"})).is_none());
        assert!(file_tags(&serde_json::json!({"title": "   "})).is_none());
        assert!(file_tags(&serde_json::json!({})).is_none());
        assert!(file_tags(&Value::Null).is_none());
    }

    #[test]
    fn icy_title_ignores_empty_and_absent() {
        // Measured cases: Radio Nova sends an empty StreamTitle, FIP sends
        // no ICY header at all (no icy-metaint whatsoever).
        assert_eq!(icy_title(&serde_json::json!({"icy-title": ""})), None);
        assert_eq!(icy_title(&serde_json::json!({"icy-title": "   "})), None);
        assert_eq!(icy_title(&serde_json::json!({"icy-br": "128"})), None);
        assert_eq!(icy_title(&serde_json::json!({})), None);
        // `metadata` is null as long as no file is loaded.
        assert_eq!(icy_title(&Value::Null), None);
        assert_eq!(icy_title(&serde_json::json!("not an object")), None);
        // A non-textual value must not panic.
        assert_eq!(icy_title(&serde_json::json!({"icy-title": 42})), None);
    }

    #[test]
    fn icy_title_tolerates_case_and_trims() {
        assert_eq!(
            icy_title(&serde_json::json!({"Icy-Title": "  Miles Davis - So What "})).as_deref(),
            Some("Miles Davis - So What")
        );
        assert_eq!(icy_title(&serde_json::json!({"ICY-TITLE": "x"})).as_deref(), Some("x"));
    }

    #[test]
    fn the_path_property_is_observed() {
        // Without it, the core never knows which file mpv is playing, and
        // the embedded cover is never read. The core does not read the
        // path from the identity: it made a principle of never
        // interpreting it.
        assert!(OBSERVED.contains(&"path"), "without it, no embedded cover ever");
    }

    #[test]
    fn a_stream_triggers_no_extraction() {
        // Only attempted on a path with no scheme.
        assert!(embedded_cover("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        assert!(embedded_cover("http://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(embedded_cover("/does/not/exist.flac").is_none());
    }

    /// Builds a real mp3 with an embedded cover, via ffmpeg, or returns
    /// `None` if it is absent.
    ///
    /// As in `ritornello-plugin-files::duration`: no binary checked into
    /// the repo, and the test is skipped rather than failed where ffmpeg is
    /// missing — it is a development tool, not a core dependency.
    ///
    /// `source_image` is an `lavfi` filter, hence the embedded image.
    /// `embedded_cover` no longer writes anything to a shared location, so
    /// two parallel tests embedding the same image can no longer collide the
    /// way an earlier version of this fixture had to guard against; callers
    /// nonetheless still pass their own filter, which is convenient to keep
    /// assertions about `content` unambiguous per test.
    ///
    /// `pub(crate)` so `cover::tests` can build a fixture that carries a
    /// real embedded picture, rather than a second copy of this function
    /// drifting apart from this one.
    pub(crate) fn mp3_with_cover_from(dir: &Path, source_image: &str) -> Option<std::path::PathBuf> {
        let image = dir.join("cover.jpg");
        let output = dir.join("with_cover.mp3");
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
                .arg(&output)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        ok.then_some(output)
    }

    /// This module's image.
    fn mp3_with_cover(dir: &Path) -> Option<std::path::PathBuf> {
        mp3_with_cover_from(dir, "color=c=red:s=16x16:d=1")
    }

    #[test]
    fn an_embedded_cover_is_probed_without_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let Some(f) = mp3_with_cover(dir.path()) else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        let before = temp_cover_files();
        let r = embedded_cover(f.to_str().unwrap()).expect("an embedded cover was expected");
        let crate::cover::CoverSource::Embedded { audio, content } = r else {
            panic!("an embedded cover must yield CoverSource::Embedded");
        };
        assert_eq!(audio, std::path::Path::new(f.to_str().unwrap()));
        assert!(!content.is_empty());
        // The point of the whole rework: nothing is written to the temp dir.
        assert_eq!(temp_cover_files(), before, "the probe must write no file");
    }

    /// Files left in the system temp dir by a previous design. Kept as a
    /// guard: this set must never grow.
    fn temp_cover_files() -> Vec<std::path::PathBuf> {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return Vec::new() };
        let mut v: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ritornello-cover-"))
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn two_tracks_of_the_same_album_share_one_cache_key() {
        let dir = tempfile::tempdir().unwrap();
        // An image of its own for this test: see `mp3_with_cover_from`.
        let Some(track1) = mp3_with_cover_from(dir.path(), "color=c=blue:s=24x24:d=1") else {
            eprintln!("ffmpeg missing: skipping test");
            return;
        };
        // Two distinct track files carrying the same cover: the common
        // case of an album, and the one path-based naming used to charge
        // fifteen times over for a single image.
        let track2 = dir.path().join("track_2.mp3");
        std::fs::copy(&track1, &track2).unwrap();

        let r1 = embedded_cover(track1.to_str().unwrap()).expect("a cover was expected");
        let r2 = embedded_cover(track2.to_str().unwrap()).expect("a cover was expected");
        let crate::cover::CoverSource::Embedded { content: c1, .. } = &r1 else {
            panic!("an embedded cover must yield CoverSource::Embedded");
        };
        let crate::cover::CoverSource::Embedded { content: c2, .. } = &r2 else {
            panic!("an embedded cover must yield CoverSource::Embedded");
        };
        assert_eq!(c1, c2, "two tracks with an identical embedded cover must yield the same content");
        // The two `audio` paths differ (track1, track2): the dedup that
        // matters is `cover::key`'s, which hashes the content, never the
        // audio path — see its doc.
        assert_eq!(
            crate::cover::key(&r1),
            crate::cover::key(&r2),
            "two tracks with an identical cover must share the same cache key"
        );
    }

    #[test]
    fn all_useful_properties_are_observed() {
        // Without `observe_property`, mpv never pushes the property: the
        // ICY layer would stay silent without any `icy_title` test
        // noticing. Since `start` launches a real mpv process, it is the
        // list it iterates over that is checked here.
        assert!(OBSERVED.contains(&"metadata"), "without it, no ICY title ever arrives");
        assert!(OBSERVED.contains(&"idle-active"), "without it, no more restart after a drop");
        assert!(OBSERVED.contains(&"media-title"));
        assert!(OBSERVED.contains(&"playlist-pos"));
    }

    #[test]
    fn an_absent_variable_gives_the_default_silently() {
        assert_eq!(audio_buffer_setting(None), AUDIO_BUFFER_DEFAULT);
        assert_eq!(readahead_setting(None), READAHEAD_DEFAULT);
    }

    #[test]
    fn a_valid_value_is_kept() {
        assert_eq!(audio_buffer_setting(Some("1.5")), 1.5);
        assert_eq!(audio_buffer_setting(Some("  2  ")), 2.0);
        assert_eq!(readahead_setting(Some("30")), 30.0);
        // 0 is legitimate: it is how to fall back to the most responsive
        // behavior, at the cost of robustness.
        assert_eq!(audio_buffer_setting(Some("0")), 0.0);
    }

    #[test]
    fn an_invalid_value_falls_back_to_the_default() {
        for raw in ["", "abc", "-1", "1,5", "NaN", "inf"] {
            assert_eq!(audio_buffer_setting(Some(raw)), AUDIO_BUFFER_DEFAULT, "raw={raw:?}");
        }
        // Beyond the upper bound: mpv would refuse above 10 s for the
        // output buffer, and an oversized read-ahead costs memory with no
        // benefit.
        assert_eq!(audio_buffer_setting(Some("42")), AUDIO_BUFFER_DEFAULT);
        assert_eq!(readahead_setting(Some("999")), READAHEAD_DEFAULT);
    }

    #[test]
    fn the_defaults_reproduce_mpvs_own() {
        // mpv 0.37: --audio-buffer=0.2 and --demuxer-readahead-secs=1
        // (measured via `mpv --list-options`). This module makes both
        // configurable without changing the default behavior: with no
        // variable set, mpv must behave exactly as if launched without
        // these options. Any drift in these values is an audio behavior
        // change that must be intentional, not a side effect — hence this
        // test.
        assert_eq!(audio_buffer_setting(None), 0.2);
        assert_eq!(readahead_setting(None), 1.0);
    }

    #[test]
    fn the_arguments_carry_both_buffers() {
        let args = mpv_args(std::path::Path::new("/run/rp/mpv.sock"), "/dev/sr0", 0.5, 10.0);
        assert!(args.contains(&"--audio-buffer=0.5".to_string()), "{args:?}");
        assert!(args.contains(&"--demuxer-readahead-secs=10".to_string()), "{args:?}");
        // The pre-existing arguments must not have been lost along the way.
        assert!(args.contains(&"--idle=yes".to_string()));
        assert!(args.contains(&"--no-video".to_string()));
        assert!(args.contains(&"--no-terminal".to_string()));
        assert!(args.contains(&"--input-ipc-server=/run/rp/mpv.sock".to_string()));
        assert!(args.contains(&"--cdda-device=/dev/sr0".to_string()));
    }

    /// mpv answers `null` on `time-pos` when nothing is loaded, and an
    /// **error** when the property is unavailable. Both say the same
    /// thing — "I don't know" — and neither is a failure to surface: an
    /// unknown position is a normal case, not an incident.
    #[test]
    fn an_absent_or_null_value_becomes_none() {
        assert_eq!(number_or_none(Ok(serde_json::json!(87.4))), Some(87.4));
        assert_eq!(number_or_none(Ok(serde_json::Value::Null)), None);
        assert_eq!(number_or_none(Err(anyhow::anyhow!("property unavailable"))), None);
    }

    /// A negative position does not exist, and mpv briefly produces one at
    /// a file's startup (measured: `-0.02`). Publishing it would display a
    /// bar going backwards.
    #[test]
    fn a_negative_value_becomes_none() {
        assert_eq!(number_or_none(Ok(serde_json::json!(-0.02))), None);
    }
}
