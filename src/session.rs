use crate::{
    access::{AccessPolicy, verify_pin},
    content::ContentSource,
    duration::{format_duration, remaining_until},
};
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{Html, IntoResponse, Response},
    routing::get,
};
use indicatif::HumanBytes;
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMode {
    Local,
    Global,
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub display_name: String,
    pub download_name: String,
    pub content_kind: &'static str,
    pub transport_label: &'static str,
    pub input_size: u64,
    pub content_length: Option<u64>,
    pub expires_at: SystemTime,
    pub remaining: Duration,
    pub once: bool,
    pub requires_pin: bool,
    pub revealed_pin: Option<String>,
    pub public_link: String,
    pub provider_status: String,
    pub completed_downloads: u64,
    pub bytes_served: u64,
    pub active_download: bool,
    pub consumed: bool,
    pub warnings: Vec<String>,
    pub mode: SessionMode,
    pub token: String,
}

#[derive(Clone, Debug)]
struct MutableSessionState {
    public_link: String,
    provider_status: String,
    completed_downloads: u64,
    bytes_served: u64,
    active_download: bool,
    consumed: bool,
}

pub struct SharedSession {
    token: String,
    content: ContentSource,
    access: AccessPolicy,
    revealed_pin: Option<String>,
    mode: SessionMode,
    shutdown: CancellationToken,
    state: Mutex<MutableSessionState>,
}

#[derive(Deserialize)]
struct PageQuery {
    token: String,
}

#[derive(Deserialize)]
struct DownloadQuery {
    pin: Option<String>,
}

#[derive(Debug)]
enum SessionRejection {
    InvalidToken,
    Expired,
    PinRequired,
    InvalidPin,
    Busy,
    AlreadyConsumed,
}

impl SharedSession {
    pub fn new(
        token: String,
        content: ContentSource,
        access: AccessPolicy,
        revealed_pin: Option<String>,
        mode: SessionMode,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            token,
            content,
            access,
            revealed_pin,
            mode,
            shutdown,
            state: Mutex::new(MutableSessionState {
                public_link: String::new(),
                provider_status: "Preparing session".to_string(),
                completed_downloads: 0,
                bytes_served: 0,
                active_download: false,
                consumed: false,
            }),
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn expires_at(&self) -> SystemTime {
        self.access.expires_at
    }

    pub fn content(&self) -> &ContentSource {
        &self.content
    }

    pub async fn set_public_link(&self, public_link: String) {
        let mut state = self.state.lock().await;
        state.public_link = public_link;
    }

    pub async fn set_provider_status(&self, status: impl Into<String>) {
        let mut state = self.state.lock().await;
        state.provider_status = status.into();
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock().await;
        SessionSnapshot {
            display_name: self.content.display_name().to_string(),
            download_name: self.content.download_name().to_string(),
            content_kind: self.content.kind_label(),
            transport_label: match self.mode {
                SessionMode::Local => "HTTPS (temporary local certificate)",
                SessionMode::Global => "HTTPS tunnel",
            },
            input_size: self.content.input_size(),
            content_length: self.content.content_length(),
            expires_at: self.access.expires_at,
            remaining: remaining_until(self.access.expires_at),
            once: self.access.once,
            requires_pin: self.access.pin_hash.is_some(),
            revealed_pin: self.revealed_pin.clone(),
            public_link: state.public_link.clone(),
            provider_status: state.provider_status.clone(),
            completed_downloads: state.completed_downloads,
            bytes_served: state.bytes_served,
            active_download: state.active_download,
            consumed: state.consumed,
            warnings: self.content.warnings().to_vec(),
            mode: self.mode,
            token: self.token.clone(),
        }
    }

    async fn build_page(&self, supplied_token: &str) -> Result<String, SessionRejection> {
        self.validate_token(supplied_token)?;

        if self.is_expired() {
            return Err(SessionRejection::Expired);
        }

        let snapshot = self.snapshot().await;
        let badge = match snapshot.mode {
            SessionMode::Local => "LAN",
            SessionMode::Global => "GLOBAL BETA",
        };
        let pin_block = if snapshot.requires_pin {
            r#"<label for="pin">PIN</label><input id="pin" name="pin" placeholder="123456" inputmode="numeric" />"#
        } else {
            ""
        };
        let size_label = match snapshot.content_length {
            Some(size) => HumanBytes(size).to_string(),
            None => format!("{} input", HumanBytes(snapshot.input_size)),
        };
        let expiry_seconds = snapshot
            .expires_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_else(|_| Duration::ZERO)
            .as_secs();
        let remaining = format_duration(snapshot.remaining);
        let warnings = if snapshot.warnings.is_empty() {
            String::new()
        } else {
            format!("<p class=\"warning\">{}</p>", snapshot.warnings.join(" · "))
        };

        Ok(format!(
            r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Beam · {name}</title>
  <style>
    :root {{
      color-scheme: light dark;
      --bg: #09111b;
      --card: rgba(17, 29, 46, 0.92);
      --line: rgba(148, 163, 184, 0.18);
      --text: #ecf4ff;
      --muted: #9eb0c5;
      --accent: #ffbe55;
      --ok: #7bdcb5;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      min-height: 100vh;
      display: grid;
      place-items: center;
      font-family: ui-rounded, system-ui, sans-serif;
      background:
        radial-gradient(circle at top left, rgba(255,190,85,0.18), transparent 32%),
        radial-gradient(circle at bottom right, rgba(123,220,181,0.18), transparent 26%),
        linear-gradient(135deg, #06111d 0%, #0b1726 48%, #050b12 100%);
      color: var(--text);
      padding: 24px;
    }}
    .card {{
      width: min(100%, 520px);
      border: 1px solid var(--line);
      background: var(--card);
      border-radius: 24px;
      padding: 28px;
      box-shadow: 0 28px 72px rgba(0,0,0,0.35);
      backdrop-filter: blur(20px);
    }}
    .eyebrow {{
      display: inline-flex;
      gap: 8px;
      align-items: center;
      color: var(--accent);
      font-size: 0.78rem;
      letter-spacing: 0.18em;
      text-transform: uppercase;
      margin-bottom: 12px;
    }}
    h1 {{
      margin: 0 0 8px;
      font-size: clamp(2rem, 7vw, 2.7rem);
      line-height: 1.05;
    }}
    p {{
      color: var(--muted);
      line-height: 1.55;
    }}
    .meta {{
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 12px;
      margin: 20px 0;
    }}
    .meta div {{
      border: 1px solid var(--line);
      border-radius: 18px;
      padding: 14px 16px;
      background: rgba(10, 19, 31, 0.6);
    }}
    .meta span {{
      display: block;
      color: var(--muted);
      font-size: 0.82rem;
      margin-bottom: 6px;
    }}
    form {{
      display: grid;
      gap: 12px;
      margin-top: 22px;
    }}
    label {{
      font-size: 0.88rem;
      color: var(--muted);
    }}
    input, button {{
      width: 100%;
      border-radius: 16px;
      border: 1px solid var(--line);
      padding: 14px 16px;
      font: inherit;
    }}
    input {{
      background: rgba(4, 9, 17, 0.82);
      color: var(--text);
    }}
    button {{
      background: linear-gradient(135deg, #ffbe55 0%, #ffd07a 100%);
      color: #231300;
      font-weight: 700;
      cursor: pointer;
    }}
    .warning {{
      margin-top: 12px;
      color: #ffd589;
    }}
    .footer {{
      margin-top: 18px;
      display: flex;
      justify-content: space-between;
      gap: 12px;
      color: var(--muted);
      font-size: 0.88rem;
    }}
  </style>
</head>
<body>
  <article class="card">
    <div class="eyebrow">Beam ⚡️ <span>{badge}</span></div>
    <h1>{name}</h1>
    <p>Zero-install download. This Beam session self-destructs in <strong id="ttl">{remaining}</strong>.</p>
    <section class="meta">
      <div><span>Payload</span><strong>{kind}</strong></div>
      <div><span>Size</span><strong>{size}</strong></div>
      <div><span>Downloads</span><strong>{downloads}</strong></div>
      <div><span>Transport</span><strong>{transport}</strong></div>
    </section>
    <form method="get" action="/download/{token}">
      {pin_block}
      <button type="submit">Download now</button>
    </form>
    {warnings}
    <div class="footer">
      <span>{status}</span>
      <span>{once_label}</span>
    </div>
  </article>
  <script>
    const ttlNode = document.getElementById("ttl");
    const expiry = {expiry_seconds} * 1000;
    const format = (ms) => {{
      if (ms <= 0) return "expired";
      const total = Math.floor(ms / 1000);
      const h = Math.floor(total / 3600);
      const m = Math.floor((total % 3600) / 60);
      const s = total % 60;
      const parts = [];
      if (h) parts.push(`${{h}}h`);
      if (m) parts.push(`${{m}}m`);
      if (s || !parts.length) parts.push(`${{s}}s`);
      return parts.join(" ");
    }};
    setInterval(() => {{
      ttlNode.textContent = format(expiry - Date.now());
    }}, 1000);
  </script>
</body>
</html>"#,
            name = snapshot.display_name,
            badge = badge,
            remaining = remaining,
            kind = snapshot.content_kind,
            size = size_label,
            downloads = snapshot.completed_downloads,
            transport = snapshot.transport_label,
            token = snapshot.token,
            status = snapshot.provider_status,
            once_label = if snapshot.once {
                "burn after reading"
            } else {
                "available until expiry"
            },
        ))
    }

    async fn begin_download(
        &self,
        supplied_token: &str,
        supplied_pin: Option<&str>,
    ) -> Result<(), SessionRejection> {
        self.validate_token(supplied_token)?;

        if self.is_expired() {
            return Err(SessionRejection::Expired);
        }

        if !verify_pin(self.access.pin_hash.as_deref(), supplied_pin) {
            return if supplied_pin.is_none() {
                Err(SessionRejection::PinRequired)
            } else {
                Err(SessionRejection::InvalidPin)
            };
        }

        let mut state = self.state.lock().await;
        if state.consumed {
            return Err(SessionRejection::AlreadyConsumed);
        }

        if self.access.once && state.active_download {
            return Err(SessionRejection::Busy);
        }

        if self.access.once {
            state.active_download = true;
        }

        Ok(())
    }

    pub async fn record_bytes(&self, count: u64) {
        let mut state = self.state.lock().await;
        state.bytes_served += count;
    }

    pub async fn finish_download(&self, success: bool) {
        let mut state = self.state.lock().await;
        if success {
            state.completed_downloads += 1;
            if self.access.once {
                state.consumed = true;
                state.active_download = false;
                state.provider_status = "Burn-after-reading completed".to_string();
                self.shutdown.cancel();
                return;
            }
        }

        if self.access.once {
            state.active_download = false;
        }
    }

    fn validate_token(&self, supplied_token: &str) -> Result<(), SessionRejection> {
        if supplied_token == self.token {
            Ok(())
        } else {
            Err(SessionRejection::InvalidToken)
        }
    }

    fn is_expired(&self) -> bool {
        self.access.expires_at <= SystemTime::now()
    }
}

pub fn build_router(session: Arc<SharedSession>) -> Router {
    Router::new()
        .route("/", get(page_handler))
        .route("/download/{token}", get(download_handler))
        .route("/healthz", get(health_handler))
        .with_state(session)
}

async fn page_handler(
    State(session): State<Arc<SharedSession>>,
    Query(query): Query<PageQuery>,
) -> Response {
    match session.build_page(&query.token).await {
        Ok(page) => Html(page).into_response(),
        Err(rejection) => rejection.into_response(),
    }
}

async fn download_handler(
    State(session): State<Arc<SharedSession>>,
    Path(token): Path<String>,
    Query(query): Query<DownloadQuery>,
) -> Response {
    if let Err(rejection) = session.begin_download(&token, query.pin.as_deref()).await {
        return rejection.into_response();
    }

    let (tx, rx) = mpsc::channel(8);
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<u64>();
    let session_for_task = session.clone();
    let content = session.content().clone();
    let session_for_progress = session.clone();

    tokio::spawn(async move {
        let mut last_total = 0_u64;
        while let Some(total) = progress_rx.recv().await {
            let delta = total.saturating_sub(last_total);
            last_total = total;
            if delta > 0 {
                session_for_progress.record_bytes(delta).await;
            }
        }
    });

    tokio::spawn(async move {
        let outcome = content.stream_to_channel(tx, progress_tx).await;
        match outcome {
            Ok(_) => session_for_task.finish_download(true).await,
            Err(_) => session_for_task.finish_download(false).await,
        }
    });

    let mut response = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );

    if let Ok(value) =
        HeaderValue::from_str(&content_disposition(session.content().download_name()))
    {
        response.headers_mut().insert(CONTENT_DISPOSITION, value);
    }

    if let Some(length) = session.content().content_length() {
        if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
            response.headers_mut().insert(CONTENT_LENGTH, value);
        }
    }

    response
}

async fn health_handler() -> &'static str {
    "ok"
}

impl IntoResponse for SessionRejection {
    fn into_response(self) -> Response {
        match self {
            SessionRejection::InvalidToken => {
                (StatusCode::NOT_FOUND, "Beam session not found").into_response()
            }
            SessionRejection::Expired => {
                (StatusCode::GONE, "This Beam session has expired").into_response()
            }
            SessionRejection::PinRequired => {
                (StatusCode::UNAUTHORIZED, "PIN required").into_response()
            }
            SessionRejection::InvalidPin => {
                (StatusCode::UNAUTHORIZED, "Invalid PIN").into_response()
            }
            SessionRejection::Busy => (
                StatusCode::CONFLICT,
                "Another burn-after-reading download is already in progress",
            )
                .into_response(),
            SessionRejection::AlreadyConsumed => {
                (StatusCode::GONE, "This Beam session was already consumed").into_response()
            }
        }
    }
}

fn content_disposition(file_name: &str) -> String {
    let escaped = file_name.replace('"', "");
    format!("attachment; filename=\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::{SessionMode, SharedSession, build_router};
    use crate::{
        access::build_access_policy,
        content::{ArchiveFormat, ContentSource},
    };
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::{fs, time::Duration};
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_local_file() {
        let temp = tempdir().unwrap();
        let file = temp.path().join("hello.txt");
        fs::write(&file, b"beam").unwrap();
        let content = ContentSource::inspect(&file, ArchiveFormat::Zip).unwrap();
        let access = build_access_policy(Duration::from_secs(300), false, None);
        let session = SharedSession::new(
            "token123".to_string(),
            content,
            access.policy,
            access.revealed_pin,
            SessionMode::Local,
            CancellationToken::new(),
        );
        let router = build_router(session);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/download/token123")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"beam");
    }

    #[tokio::test]
    async fn rejects_incorrect_pin() {
        let temp = tempdir().unwrap();
        let file = temp.path().join("secret.txt");
        fs::write(&file, b"beam").unwrap();
        let content = ContentSource::inspect(&file, ArchiveFormat::Zip).unwrap();
        let access =
            build_access_policy(Duration::from_secs(300), false, Some("123456".to_string()));
        let session = SharedSession::new(
            "token123".to_string(),
            content,
            access.policy,
            access.revealed_pin,
            SessionMode::Local,
            CancellationToken::new(),
        );
        let router = build_router(session);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/download/token123?pin=000000")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
