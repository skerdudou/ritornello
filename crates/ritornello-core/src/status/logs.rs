//! Logs and stream: the last log lines for the System page, the buffer that retains them, and the SSE stream of the player state.

use super::*;

#[derive(Serialize)]
pub(super) struct LogsResponse {
    lines: Vec<String>,
}

/// The last WARN/ERROR lines, most recent first — that is the order in which
/// the old status page displayed them.
pub(super) async fn logs_json(State(state): State<AppState>) -> Json<LogsResponse> {
    let mut lines = state.logs.snapshot();
    lines.reverse();
    Json(LogsResponse { lines })
}

/// The whole journal — INFO and above, the core's and the plugins' — most
/// recent first, like `/api/logs`. Its own route rather than a parameter of
/// that one: the card of recent errors is fetched on every visit of the System
/// page, the journal only when someone opens it, and it weighs up to a few
/// hundred kilobytes.
pub(super) async fn journal_json(State(state): State<AppState>) -> Json<LogsResponse> {
    let mut lines = state.logs.journal_snapshot();
    lines.reverse();
    Json(LogsResponse { lines })
}

/// Player state as a pushed stream (`text/event-stream`): active source, volume,
/// mute, standby, and the track when it is known.
///
/// Everything **volatile** goes through here, and nothing else: that is why the
/// volume is exposed by no polled route. `/api/status` carries, alongside, the
/// navigation contract (which plugins exist, which ones have an admin page),
/// structurally stable and read once at mount.
///
/// Pushed rather than polled, for three reasons measured before deciding: the
/// SPA polls nothing today (no `setInterval`, no WebSocket); the core
/// **already** broadcasts its changes on a `watch` channel, so the route costs
/// only a few lines and adds no state; and a device that is idle most of the
/// time should not receive requests that teach nothing. Useful corollary: the
/// displayed volume follows the infrared remote and the other tabs, which
/// polling would only have given with one interval of lag.
///
/// The current state is emitted **as soon as the connection opens** — same
/// property as the OUI FM stream consumed elsewhere: a tab opened in the middle
/// of a track must not stay blank until the next one.
///
/// No authentication, like all the other routes of the device: adding some here
/// alone would only give the illusion of protection while `/api/command`
/// already drives playback without asking for any.
pub(super) async fn player_sse(
    State(state): State<AppState>,
) -> axum::response::Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>
{
    use futures::StreamExt;

    let stream = futures::stream::unfold((state.player.clone(), true), |(mut rx, first)| async move {
        if first {
            // `borrow_and_update` marks the value as seen: the next `changed()`
            // will wait for a real change instead of returning the state
            // already emitted right away.
            let state = rx.borrow_and_update().clone();
            return Some((state, (rx, false)));
        }
        // Err = the core dropped the sender: end of stream, the browser will
        // reconnect on its own (`EventSource` takes care of it).
        rx.changed().await.ok()?;
        let state = rx.borrow_and_update().clone();
        Some((state, (rx, false)))
    })
    .map(|state| {
        // Serializing a `PlayerState` cannot fail (only simple types); in case
        // of the unexpected, an empty object beats a cut connection, which the
        // client would interpret as a failure.
        Ok(axum::response::sse::Event::default()
            .json_data(&state)
            .unwrap_or_else(|_| axum::response::sse::Event::default().data("{}")))
    });

    axum::response::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// The log lines kept in memory for the System page, in **two rings**.
///
/// - `errors`: WARN and ERROR only, what the "Recent errors" card shows.
/// - `journal`: everything from INFO up, for the "Full journal" dialog.
///
/// Two rings and not one ring filtered on read: the journal is chatty (a line
/// per track, per cover, per plugin announcement), and in a single ring of any
/// reasonable size it would push out the one error worth reading before
/// anyone looked. Each ring evicts only its own kind.
///
/// Both receive the core's own lines from `tracing` layers installed in `main`
/// (`LogBufferWriter`, `JournalWriter`), and the plugins' lines from the relay
/// in `plugins::spawn` (`record`). On the device the journald copy is the one
/// that survives a restart; this is the one the owner can read without a
/// shell, and the plugins' lines used to reach only journald — a refusal to
/// archive a cover, for instance, was invisible from the page.
#[derive(Debug)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
    journal: Mutex<VecDeque<String>>,
    journal_capacity: usize,
}

/// A log line is cut beyond this many bytes, so that the journal's memory
/// stays bounded by its line count whatever a plugin prints: 5000 lines of
/// at most 1 KB is 5 MB at the very worst, and a few hundred KB in practice.
pub const MAX_LINE_BYTES: usize = 1024;

fn push_bounded(ring: &Mutex<VecDeque<String>>, capacity: usize, line: String) {
    let mut lines = ring.lock().unwrap();
    if lines.len() == capacity {
        lines.pop_front();
    }
    lines.push_back(line);
}

impl LogBuffer {
    /// `capacity` lines of errors, and as many of journal: what tests want.
    /// The service sizes the journal on its own with `with_journal`.
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            journal: Mutex::new(VecDeque::new()),
            journal_capacity: capacity,
        }
    }

    pub fn with_journal(mut self, capacity: usize) -> Self {
        self.journal_capacity = capacity;
        self
    }

    /// An error line (WARN/ERROR), into the errors ring only: the core's
    /// errors reach the journal through their own layer.
    pub fn push(&self, line: String) {
        push_bounded(&self.lines, self.capacity, line);
    }

    /// A line into the journal ring only.
    pub fn push_journal(&self, line: String) {
        push_bounded(&self.journal, self.journal_capacity, line);
    }

    /// A line that has no `tracing` layer to sort it — a plugin's — into the
    /// journal, and into the errors ring too when it is one.
    pub fn record(&self, line: String, is_error: bool) {
        if is_error {
            self.push(line.clone());
        }
        self.push_journal(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }

    pub fn journal_snapshot(&self) -> Vec<String> {
        self.journal.lock().unwrap().iter().cloned().collect()
    }
}

/// `io::Write` adapter to plug `LogBuffer` in as the output of a
/// `tracing_subscriber::fmt::layer()` layer: the errors ring.
pub struct LogBufferWriter(pub Arc<LogBuffer>);

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(line) = layer_line(buf) {
            self.0.push(line);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The same adapter for the journal ring.
pub struct JournalWriter(pub Arc<LogBuffer>);

impl std::io::Write for JournalWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(line) = layer_line(buf) {
            self.0.push_journal(line);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One formatted event, as a ring wants it: trimmed, non-empty, and cut to
/// `MAX_LINE_BYTES`.
fn layer_line(buf: &[u8]) -> Option<String> {
    let line = std::str::from_utf8(buf).ok()?.trim_end();
    (!line.is_empty()).then(|| truncate_line(line).to_string())
}

/// `line` cut to `MAX_LINE_BYTES`, on a character boundary.
pub fn truncate_line(line: &str) -> &str {
    if line.len() <= MAX_LINE_BYTES {
        return line;
    }
    let mut end = MAX_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn api_logs_returns_the_most_recent_lines_first() {
        let state = tests_support::app_state();
        state.logs.push("WARN first".into());
        state.logs.push("WARN second".into());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/logs").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lines: Vec<String> = serde_json::from_value(v["lines"].clone()).unwrap();
        // Reverse order, as the server-rendered page did.
        assert_eq!(lines, vec!["WARN second".to_string(), "WARN first".to_string()]);
    }

    #[tokio::test]
    async fn api_journal_returns_the_whole_journal_most_recent_first() {
        let state = tests_support::app_state();
        state.logs.push_journal("INFO first".into());
        state.logs.record("WARN second".into(), true);
        state.logs.record("INFO third".into(), false);
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/journal").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lines: Vec<String> = serde_json::from_value(v["lines"].clone()).unwrap();
        assert_eq!(lines, vec!["INFO third", "WARN second", "INFO first"]);
    }

    #[test]
    fn the_journal_never_pushes_an_error_out_of_its_own_ring() {
        // The reason for two rings: a chatty journal must not evict the one
        // error the card exists to show.
        let logs = LogBuffer::new(2).with_journal(3);
        logs.record("WARN the one that matters".into(), true);
        for i in 0..100 {
            logs.record(format!("INFO chatter {i}"), false);
        }
        assert_eq!(logs.snapshot(), vec!["WARN the one that matters"]);
        assert_eq!(logs.journal_snapshot(), vec!["INFO chatter 97", "INFO chatter 98", "INFO chatter 99"]);
    }

    #[test]
    fn an_error_recorded_goes_to_both_rings_a_plain_line_to_the_journal_only() {
        let logs = LogBuffer::new(5);
        logs.record("ERROR e".into(), true);
        logs.record("INFO i".into(), false);
        assert_eq!(logs.snapshot(), vec!["ERROR e"]);
        assert_eq!(logs.journal_snapshot(), vec!["ERROR e", "INFO i"]);
    }

    #[test]
    fn both_layer_writers_cut_a_line_to_its_byte_budget() {
        use std::io::Write;
        let logs = Arc::new(LogBuffer::new(5));
        let long = format!("{}\n", "x".repeat(MAX_LINE_BYTES * 3));
        LogBufferWriter(logs.clone()).write_all(long.as_bytes()).unwrap();
        JournalWriter(logs.clone()).write_all(long.as_bytes()).unwrap();
        assert_eq!(logs.snapshot()[0].len(), MAX_LINE_BYTES);
        assert_eq!(logs.journal_snapshot()[0].len(), MAX_LINE_BYTES);
    }

    /// Reads the next SSE frame from a response body.
    ///
    /// The stream is **infinite**: a `collect()` on the body would never
    /// return. So we read chunk by chunk, accumulating until a complete frame
    /// (terminated by the blank line separating SSE events), and return the
    /// payload of the `data:` line.
    async fn next_frame(body: &mut axum::body::BodyDataStream) -> serde_json::Value {
        use futures::StreamExt;
        let mut buffer = String::new();
        for _ in 0..50 {
            let Some(chunk) = body.next().await else { panic!("stream ended before the frame") };
            buffer.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            if let Some(data) = buffer.lines().find_map(|l| l.strip_prefix("data:"))
                && buffer.contains("\n\n")
            {
                return serde_json::from_str(data.trim()).expect("JSON payload");
            }
        }
        panic!("no complete frame received: {buffer:?}");
    }

    fn player_state(title: &str) -> crate::metadata::PlayerState {
        crate::metadata::PlayerState {
            source: "radio".into(),
            volume: 60,
            track: crate::metadata::Track {
                title: Some(title.into()),
                origin: Some("icy".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn player_emits_the_current_state_on_connection() {
        // Property borrowed from the OUI FM stream: a tab opened in the middle
        // of a track must not stay blank until the next one.
        let (state, _tx) = tests_support::app_state_with_player(player_state("Miles Davis - So What"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/player").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap().to_str().unwrap(),
            "text/event-stream"
        );
        let mut body = resp.into_body().into_data_stream();
        let v = next_frame(&mut body).await;
        assert_eq!(v["title"], "Miles Davis - So What");
        assert_eq!(v["source"], "radio");
        assert_eq!(v["origin"], "icy");
    }

    #[tokio::test]
    async fn player_pushes_the_following_changes() {
        let (state, tx) = tests_support::app_state_with_player(player_state("first"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/player").body(Body::empty()).unwrap()).await.unwrap();
        let mut body = resp.into_body().into_data_stream();
        assert_eq!(next_frame(&mut body).await["title"], "first");
        tx.send(player_state("second")).unwrap();
        assert_eq!(next_frame(&mut body).await["title"], "second");
    }

    #[tokio::test]
    async fn two_clients_both_receive() {
        let (state, tx) = tests_support::app_state_with_player(player_state("first"));
        let one = router(state.clone())
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let two = router(state)
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut body_one = one.into_body().into_data_stream();
        let mut body_two = two.into_body().into_data_stream();
        assert_eq!(next_frame(&mut body_one).await["title"], "first");
        assert_eq!(next_frame(&mut body_two).await["title"], "first");
        tx.send(player_state("second")).unwrap();
        assert_eq!(next_frame(&mut body_one).await["title"], "second");
        assert_eq!(next_frame(&mut body_two).await["title"], "second");
    }

    #[tokio::test]
    async fn a_client_that_disconnects_disturbs_neither_the_channel_nor_the_others() {
        let (state, tx) = tests_support::app_state_with_player(player_state("first"));
        let survivor = router(state.clone())
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut body_survivor = survivor.into_body().into_data_stream();
        assert_eq!(next_frame(&mut body_survivor).await["title"], "first");

        {
            let gone = router(state)
                .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let mut body = gone.into_body().into_data_stream();
            next_frame(&mut body).await;
            // End of scope: the body is dropped, like a closed tab.
        }

        // Emission keeps working, and the other client receives it.
        assert!(tx.send(player_state("second")).is_ok(), "the channel must not be broken");
        assert_eq!(next_frame(&mut body_survivor).await["title"], "second");
    }

    #[test]
    fn log_buffer_caps_at_50_lines() {
        let buf = LogBuffer::new(50);
        for i in 0..60 {
            buf.push(format!("line {i}"));
        }
        let lines = buf.snapshot();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "line 10"); // the 10 oldest have been evicted
        assert_eq!(lines[49], "line 59");
    }

    #[test]
    fn log_buffer_writer_pushes_complete_lines() {
        use std::io::Write;
        let buf = Arc::new(LogBuffer::new(10));
        let mut w = LogBufferWriter(buf.clone());
        writeln!(w, "WARN radio plugin unavailable").unwrap();
        assert_eq!(buf.snapshot(), vec!["WARN radio plugin unavailable".to_string()]);
    }

    /// The production capacity, not that of a test setup: the buffer must
    /// retain 500 lines and drop the oldest, otherwise the "all errors" popup
    /// of the UI has nothing more to show than the card that already displays
    /// the latest ones.
    #[test]
    fn log_buffer_retains_five_hundred_lines() {
        let buf = LogBuffer::new(500);
        for i in 0..600 {
            buf.push(format!("line {i}"));
        }
        let lines = buf.snapshot();
        assert_eq!(lines.len(), 500);
        assert_eq!(lines.first().map(String::as_str), Some("line 100"));
        assert_eq!(lines.last().map(String::as_str), Some("line 599"));
    }
}
