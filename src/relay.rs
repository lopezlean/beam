use crate::{
    access::generate_token,
    relay_protocol::{
        ClientToRelayMessage, HeaderPair, RelaySessionCreateRequest, RelaySessionCreateResponse,
        RelayToClientMessage, decode_body_chunk, headers_to_pairs, pairs_to_headers,
    },
};
use anyhow::{Context, Result};
use axum::{
    Router,
    body::Body,
    extract::{
        OriginalUri, Path, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use url::Url;

#[derive(Clone)]
pub struct RelayState {
    public_base_url: Url,
    sessions: Arc<RwLock<HashMap<String, Arc<RelaySession>>>>,
    next_request_id: Arc<AtomicU64>,
}

struct RelaySession {
    secret: String,
    expires_at_unix: u64,
    sender: Mutex<Option<mpsc::UnboundedSender<WsMessage>>>,
    pending: Mutex<HashMap<u64, PendingResponse>>,
}

struct PendingResponse {
    head_tx: Option<oneshot::Sender<RelayResponseHead>>,
    body_tx: mpsc::Sender<Result<Bytes, io::Error>>,
}

struct RelayResponseHead {
    status: u16,
    headers: Vec<HeaderPair>,
}

#[derive(Deserialize)]
struct ClientQuery {
    secret: String,
}

impl RelayState {
    pub fn new(public_base_url: Url) -> Self {
        Self {
            public_base_url,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            next_request_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn router(public_base_url: Url) -> Router {
        let state = Self::new(public_base_url);
        Router::new()
            .route("/healthz", get(health_handler))
            .route("/v1/sessions", post(create_session_handler))
            .route("/v1/client/ws/{public_id}", get(client_ws_handler))
            .route("/s/{public_id}", get(public_root_handler))
            .route("/s/{public_id}/{*tail}", get(public_tail_handler))
            .with_state(state)
    }

    fn public_url(&self, public_id: &str) -> String {
        self.public_base_url
            .join(&format!("s/{public_id}"))
            .expect("valid public relay URL")
            .to_string()
    }

    fn websocket_url(&self, public_id: &str, secret: &str) -> String {
        let mut url = self
            .public_base_url
            .join(&format!("v1/client/ws/{public_id}"))
            .expect("valid websocket relay URL");
        let scheme = match url.scheme() {
            "https" => "wss",
            _ => "ws",
        };
        url.set_scheme(scheme)
            .expect("websocket scheme should be valid");
        url.set_query(Some(&format!("secret={secret}")));
        url.to_string()
    }

    async fn create_session(
        &self,
        request: RelaySessionCreateRequest,
    ) -> RelaySessionCreateResponse {
        let public_id = generate_token();
        let secret = generate_token();
        let session = Arc::new(RelaySession {
            secret: secret.clone(),
            expires_at_unix: request.expires_at_unix,
            sender: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
        });

        self.sessions
            .write()
            .await
            .insert(public_id.clone(), session.clone());

        RelaySessionCreateResponse {
            public_id: public_id.clone(),
            public_url: self.public_url(&public_id),
            websocket_url: self.websocket_url(&public_id, &secret),
            secret,
        }
    }
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn create_session_handler(
    State(state): State<RelayState>,
    axum::Json(request): axum::Json<RelaySessionCreateRequest>,
) -> axum::Json<RelaySessionCreateResponse> {
    axum::Json(state.create_session(request).await)
}

async fn client_ws_handler(
    State(state): State<RelayState>,
    Path(public_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<ClientQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let session = match state.sessions.read().await.get(&public_id).cloned() {
        Some(session) => session,
        None => return (StatusCode::NOT_FOUND, "Beam relay session not found").into_response(),
    };

    if session.secret != query.secret {
        return (StatusCode::UNAUTHORIZED, "Invalid relay session secret").into_response();
    }

    ws.on_upgrade(move |socket| client_socket(socket, state, public_id, session))
}

async fn public_root_handler(
    State(state): State<RelayState>,
    Path(public_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    forward_public_request(state, public_id, "/".to_string(), uri.query(), headers).await
}

async fn public_tail_handler(
    State(state): State<RelayState>,
    Path((public_id, tail)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    forward_public_request(state, public_id, format!("/{tail}"), uri.query(), headers).await
}

async fn forward_public_request(
    state: RelayState,
    public_id: String,
    path: String,
    query: Option<&str>,
    headers: HeaderMap,
) -> Response {
    let Some(session) = state.sessions.read().await.get(&public_id).cloned() else {
        return (StatusCode::NOT_FOUND, "Beam relay session not found").into_response();
    };

    if session.expires_at_unix <= unix_now() {
        state.sessions.write().await.remove(&public_id);
        return (StatusCode::GONE, "Beam relay session expired").into_response();
    }

    let sender = {
        let guard = session.sender.lock().await;
        guard.clone()
    };

    let Some(sender) = sender else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Beam sender is not connected to the relay",
        )
            .into_response();
    };

    let request_id = state.next_request_id.fetch_add(1, Ordering::Relaxed);
    let (head_tx, head_rx) = oneshot::channel();
    let (body_tx, body_rx) = mpsc::channel(8);
    session.pending.lock().await.insert(
        request_id,
        PendingResponse {
            head_tx: Some(head_tx),
            body_tx,
        },
    );

    let message = RelayToClientMessage::RequestStart {
        request_id,
        method: "GET".to_string(),
        path,
        query: query.map(str::to_owned),
        headers: headers_to_pairs(&headers, relay_request_header_allowed),
    };

    if sender
        .send(WsMessage::Text(
            serde_json::to_string(&message)
                .expect("relay request should serialize")
                .into(),
        ))
        .is_err()
    {
        session.pending.lock().await.remove(&request_id);
        return (
            StatusCode::BAD_GATEWAY,
            "Beam relay could not reach the sender",
        )
            .into_response();
    }

    let head = match tokio::time::timeout(Duration::from_secs(30), head_rx).await {
        Ok(Ok(head)) => head,
        Ok(Err(_)) => {
            session.pending.lock().await.remove(&request_id);
            return (
                StatusCode::BAD_GATEWAY,
                "Beam sender closed the relay request",
            )
                .into_response();
        }
        Err(_) => {
            session.pending.lock().await.remove(&request_id);
            return (StatusCode::GATEWAY_TIMEOUT, "Beam sender timed out").into_response();
        }
    };

    let mut response = Response::new(Body::from_stream(ReceiverStream::new(body_rx)));
    *response.status_mut() = StatusCode::from_u16(head.status).unwrap_or(StatusCode::BAD_GATEWAY);
    response
        .headers_mut()
        .extend(pairs_to_headers(&head.headers).into_iter());
    response
}

async fn client_socket(
    socket: WebSocket,
    state: RelayState,
    public_id: String,
    session: Arc<RelaySession>,
) {
    let (mut sink, mut stream) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<WsMessage>();

    {
        let mut sender = session.sender.lock().await;
        *sender = Some(outbound_tx.clone());
    }

    let writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    while let Some(message_result) = stream.next().await {
        let Ok(message) = message_result else {
            break;
        };

        match message {
            WsMessage::Text(text) => {
                if handle_client_control_message(&session, text.as_str())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            WsMessage::Binary(payload) => {
                if handle_client_binary_message(&session, &payload)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            WsMessage::Close(_) => break,
            WsMessage::Ping(_) | WsMessage::Pong(_) => {}
        }
    }

    {
        let mut sender = session.sender.lock().await;
        *sender = None;
    }
    fail_all_pending(
        &session,
        StatusCode::BAD_GATEWAY,
        "Beam sender disconnected from relay".to_string(),
    )
    .await;
    state.sessions.write().await.remove(&public_id);
    drop(outbound_tx);
    let _ = writer.await;
}

async fn handle_client_control_message(session: &Arc<RelaySession>, payload: &str) -> Result<()> {
    match serde_json::from_str::<ClientToRelayMessage>(payload)
        .context("invalid relay control message")?
    {
        ClientToRelayMessage::ResponseStart {
            request_id,
            status,
            headers,
        } => {
            let mut pending = session.pending.lock().await;
            if let Some(entry) = pending.get_mut(&request_id) {
                if let Some(head_tx) = entry.head_tx.take() {
                    let _ = head_tx.send(RelayResponseHead { status, headers });
                }
            }
        }
        ClientToRelayMessage::ResponseEnd { request_id } => {
            session.pending.lock().await.remove(&request_id);
        }
        ClientToRelayMessage::ResponseError {
            request_id,
            status,
            message,
        } => {
            let entry = session.pending.lock().await.remove(&request_id);
            if let Some(mut entry) = entry {
                if let Some(head_tx) = entry.head_tx.take() {
                    let _ = head_tx.send(RelayResponseHead {
                        status,
                        headers: vec![HeaderPair {
                            name: "content-type".to_string(),
                            value: "text/plain; charset=utf-8".to_string(),
                        }],
                    });
                }
                let _ = entry.body_tx.send(Ok(Bytes::from(message))).await;
            }
        }
        ClientToRelayMessage::SessionClose { .. } => {}
    }

    Ok(())
}

async fn handle_client_binary_message(session: &Arc<RelaySession>, payload: &[u8]) -> Result<()> {
    let (request_id, bytes) = decode_body_chunk(payload)?;
    let body_tx = {
        let pending = session.pending.lock().await;
        pending.get(&request_id).map(|entry| entry.body_tx.clone())
    };

    if let Some(body_tx) = body_tx {
        let _ = body_tx.send(Ok(bytes)).await;
    }

    Ok(())
}

async fn fail_all_pending(session: &Arc<RelaySession>, status: StatusCode, message: String) {
    let pending = {
        let mut pending = session.pending.lock().await;
        std::mem::take(&mut *pending)
    };

    for (_, mut entry) in pending {
        if let Some(head_tx) = entry.head_tx.take() {
            let _ = head_tx.send(RelayResponseHead {
                status: status.as_u16(),
                headers: vec![HeaderPair {
                    name: "content-type".to_string(),
                    value: "text/plain; charset=utf-8".to_string(),
                }],
            });
        }
        let _ = entry.body_tx.send(Ok(Bytes::from(message.clone()))).await;
    }
}

fn relay_request_header_allowed(name: &HeaderName) -> bool {
    !is_hop_by_hop_header(name)
}

pub fn relay_response_header_allowed(name: &HeaderName) -> bool {
    !is_hop_by_hop_header(name)
}

fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-extensions"
            | "sec-websocket-accept"
            | "sec-websocket-protocol"
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::RelayState;
    use crate::relay_protocol::RelaySessionCreateRequest;
    use url::Url;

    #[tokio::test]
    async fn creates_public_and_websocket_urls() {
        let state = RelayState::new(Url::parse("http://127.0.0.1:8787/").unwrap());
        let response = state
            .create_session(RelaySessionCreateRequest {
                download_name: "file.txt".to_string(),
                expires_at_unix: 123,
            })
            .await;

        assert!(response.public_url.starts_with("http://127.0.0.1:8787/s/"));
        assert!(
            response
                .websocket_url
                .starts_with("ws://127.0.0.1:8787/v1/client/ws/")
        );
    }
}
