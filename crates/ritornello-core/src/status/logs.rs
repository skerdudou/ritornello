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

/// Ring buffer of the last log lines (WARN/ERROR), displayed on the status
/// page. `LogBufferWriter` (below) pushes lines into it from a `tracing` layer
/// installed in `main`.
#[derive(Debug)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { lines: Mutex::new(VecDeque::with_capacity(capacity)), capacity }
    }

    pub fn push(&self, line: String) {
        let mut lines = self.lines.lock().unwrap();
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }
}

/// `io::Write` adapter to plug `LogBuffer` in as the output of a
/// `tracing_subscriber::fmt::layer()` layer (see Task 8).
pub struct LogBufferWriter(pub Arc<LogBuffer>);

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(s) = std::str::from_utf8(buf) {
            let line = s.trim_end();
            if !line.is_empty() {
                self.0.push(line.to_string());
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
            if let Some(data) = buffer.lines().find_map(|l| l.strip_prefix("data:")) {
                if buffer.contains("\n\n") {
                    return serde_json::from_str(data.trim()).expect("JSON payload");
                }
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
