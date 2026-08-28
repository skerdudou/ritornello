//! Journaux et stream : les dernieres lines de log pour la page Systeme, le buffer qui les retient, et le stream SSE de l'state du player.

use super::*;

#[derive(Serialize)]
pub(super) struct LogsResponse {
    lines: Vec<String>,
}

/// Les dernières lines WARN/ERROR, les plus récentes en premier — c'est
/// l'order dans lequel l'ancienne page de statut les affichait.
pub(super) async fn logs_json(State(state): State<AppState>) -> Json<LogsResponse> {
    let mut lines = state.logs.snapshot();
    lines.reverse();
    Json(LogsResponse { lines })
}

/// État du player en stream poussé (`text/event-stream`) : source active, volume,
/// muet, veille, et le track quand on le connaît.
///
/// Tout ce qui est **volatil** passe ici, et rien d'autre : c'est la raison pour
/// laquelle le volume n'est exposé par aucune route sondée. `/api/status` porte à
/// côté le contrat de navigation (quels plugins existent, lesquels ont une page
/// d'admin), structurellement stable et lu une fois au montage.
///
/// Poussé et non sondé, pour trois raisons mesurées avant de trancher : la SPA
/// ne sonde rien aujourd'hui (aucun `setInterval`, aucun WebSocket) ; le cœur
/// diffuse **déjà** ses changements sur un canal `watch`, donc la route ne
/// coûte que quelques lines et n'add aucun état ; et un appareil le plus
/// souvent inactif n'a pas à recevoir des requêtes qui n'apprennent rien.
/// Corollaire utile : le volume affiché suit la télécommande infrarouge et les
/// autres onglets, ce qu'un sondage n'aurait donné qu'avec un intervalle de
/// retard.
///
/// L'état courant est émis **dès la connexion** — même propriété que le stream
/// d'OUI FM qu'on consomme par ailleurs : un onglet ouvert au milieu d'un
/// track ne doit pas rester clear jusqu'au suivant.
///
/// Pas d'authentification, comme toutes les autres routes de l'appareil : en
/// ajouter ici seulement donnerait l'illusion d'une protection alors que
/// `/api/command` pilote déjà la playback sans en demander.
pub(super) async fn player_sse(
    State(state): State<AppState>,
) -> axum::response::Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>>
{
    use futures::StreamExt;

    let stream = futures::stream::unfold((state.player.clone(), true), |(mut rx, premier)| async move {
        if premier {
            // `borrow_and_update` marque la valeur comme vue : le prochain
            // `changed()` attendra un vrai changement au lieu de renvoyer
            // aussitôt l'état déjà émis.
            let state = rx.borrow_and_update().clone();
            return Some((state, (rx, false)));
        }
        // Err = le cœur a lâché l'émetteur : fin du stream, le navigateur
        // reconnectera de lui-même (`EventSource` s'en charge).
        rx.changed().await.ok()?;
        let state = rx.borrow_and_update().clone();
        Some((state, (rx, false)))
    })
    .map(|state| {
        // La sérialisation d'un `PlayerState` ne peut pas échouer (que des
        // types simples) ; en cas d'imprévu, un objet clear vaut mieux qu'une
        // connexion coupée, que le client interpréterait comme une panne.
        Ok(axum::response::sse::Event::default()
            .json_data(&state)
            .unwrap_or_else(|_| axum::response::sse::Event::default().data("{}")))
    });

    axum::response::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Tampon circulaire des dernières lines de log (WARN/ERROR), affiché sur
/// la page de statut. `LogBufferWriter` (ci-dessous) y push_cover les lines
/// depuis une couche `tracing` installée dans `main`.
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

/// Adaptateur `io::Write` pour brancher `LogBuffer` comme sortie d'une
/// couche `tracing_subscriber::fmt::layer()` (voir Task 8).
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
    async fn api_logs_renvoie_les_lignes_les_plus_recentes_en_premier() {
        let state = tests_support::app_state();
        state.logs.push("WARN premiere".into());
        state.logs.push("WARN seconde".into());
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/logs").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lines: Vec<String> = serde_json::from_value(v["lines"].clone()).unwrap();
        // Ordre inverse, comme le faisait la page rendue cote serveur.
        assert_eq!(lines, vec!["WARN seconde".to_string(), "WARN premiere".to_string()]);
    }

    /// Lit la prochaine trame SSE d'un corps de réponse.
    ///
    /// Le stream est **infini** : un `collect()` sur le corps ne rendrait jamais
    /// la main. On read donc track par track, en accumulant jusqu'à une
    /// trame complète (terminée par la line clear qui sépare les événements
    /// SSE), et on renvoie la charge utile de la line `data:`.
    async fn prochaine_trame(corps: &mut axum::body::BodyDataStream) -> serde_json::Value {
        use futures::StreamExt;
        let mut buffer = String::new();
        for _ in 0..50 {
            let Some(chunk) = corps.next().await else { panic!("stream terminate avant la trame") };
            buffer.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
            if let Some(data) = buffer.lines().find_map(|l| l.strip_prefix("data:")) {
                if buffer.contains("\n\n") {
                    return serde_json::from_str(data.trim()).expect("charge utile JSON");
                }
            }
        }
        panic!("aucune trame complete recue : {buffer:?}");
    }

    fn player_state(titre: &str) -> crate::metadata::PlayerState {
        crate::metadata::PlayerState {
            source: "radio".into(),
            volume: 60,
            track: crate::metadata::Track {
                title: Some(titre.into()),
                origin: Some("icy".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn player_emet_letat_courant_des_la_connexion() {
        // Propriété reprise du stream d'OUI FM : un onglet ouvert au milieu d'un
        // track ne doit pas rester clear jusqu'au suivant.
        let (state, _tx) = tests_support::app_state_with_player(player_state("Miles Davis - So What"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/player").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap().to_str().unwrap(),
            "text/event-stream"
        );
        let mut corps = resp.into_body().into_data_stream();
        let v = prochaine_trame(&mut corps).await;
        assert_eq!(v["title"], "Miles Davis - So What");
        assert_eq!(v["source"], "radio");
        assert_eq!(v["origin"], "icy");
    }

    #[tokio::test]
    async fn player_pousse_les_changements_suivants() {
        let (state, tx) = tests_support::app_state_with_player(player_state("premier"));
        let app = router(state);
        let resp = app.oneshot(Request::get("/api/player").body(Body::empty()).unwrap()).await.unwrap();
        let mut corps = resp.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps).await["title"], "premier");
        tx.send(player_state("second")).unwrap();
        assert_eq!(prochaine_trame(&mut corps).await["title"], "second");
    }

    #[tokio::test]
    async fn deux_clients_recoivent_tous_les_deux() {
        let (state, tx) = tests_support::app_state_with_player(player_state("premier"));
        let un = router(state.clone())
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let deux = router(state)
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut corps_un = un.into_body().into_data_stream();
        let mut corps_deux = deux.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps_un).await["title"], "premier");
        assert_eq!(prochaine_trame(&mut corps_deux).await["title"], "premier");
        tx.send(player_state("second")).unwrap();
        assert_eq!(prochaine_trame(&mut corps_un).await["title"], "second");
        assert_eq!(prochaine_trame(&mut corps_deux).await["title"], "second");
    }

    #[tokio::test]
    async fn un_client_qui_se_deconnecte_ne_perturbe_ni_le_canal_ni_les_autres() {
        let (state, tx) = tests_support::app_state_with_player(player_state("premier"));
        let survivant = router(state.clone())
            .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut corps_survivant = survivant.into_body().into_data_stream();
        assert_eq!(prochaine_trame(&mut corps_survivant).await["title"], "premier");

        {
            let parti = router(state)
                .oneshot(Request::get("/api/player").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let mut corps = parti.into_body().into_data_stream();
            prochaine_trame(&mut corps).await;
            // Eof de portée : le corps est lâché, comme un onglet fermé.
        }

        // L'émission continue de fonctionner, et l'autre client la reçoit.
        assert!(tx.send(player_state("second")).is_ok(), "le canal ne doit pas etre casse");
        assert_eq!(prochaine_trame(&mut corps_survivant).await["title"], "second");
    }

    #[test]
    fn log_buffer_plafonne_a_50_lignes() {
        let buf = LogBuffer::new(50);
        for i in 0..60 {
            buf.push(format!("line {i}"));
        }
        let lines = buf.snapshot();
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0], "line 10"); // les 10 plus anciennes ont ete evincees
        assert_eq!(lines[49], "line 59");
    }

    #[test]
    fn log_buffer_writer_pousse_les_lignes_completes() {
        use std::io::Write;
        let buf = Arc::new(LogBuffer::new(10));
        let mut w = LogBufferWriter(buf.clone());
        writeln!(w, "WARN plugin radio indisponible").unwrap();
        assert_eq!(buf.snapshot(), vec!["WARN plugin radio indisponible".to_string()]);
    }

    /// La capacité de production, pas celle d'un montage de test : le buffer
    /// doit retenir 500 lines et jeter les plus anciennes, sinon la popin
    /// « toutes les erreurs » de l'IHM n'a rien de plus à montrer que la carte
    /// qui en affiche déjà les dernières.
    #[test]
    fn log_buffer_retient_cinq_cents_lignes() {
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
