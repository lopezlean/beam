use crate::{
    access::{AccessPolicy, verify_pin},
    content::ContentSource,
    download_page,
    duration::remaining_until,
    provider::ProviderKind,
};
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
        },
    },
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestedSendMode {
    Global { provider: ProviderKind },
    Local { base_port: Option<u16> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedSendMode {
    Global { provider: ProviderKind },
    Local { http_port: u16, https_port: u16 },
}

impl ResolvedSendMode {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    pub fn primary_link_label(&self) -> &'static str {
        match self {
            Self::Global { .. } => "Public HTTPS",
            Self::Local { .. } => "Primary (No Warnings)",
        }
    }

    pub fn secondary_link_label(&self) -> Option<&'static str> {
        match self {
            Self::Global { .. } => None,
            Self::Local { .. } => Some("Secondary (Encrypted)"),
        }
    }

    pub fn transport_label(&self) -> String {
        match self {
            Self::Global { provider } => provider.transport_label().to_string(),
            Self::Local { .. } => "HTTP primary + HTTPS optional".to_string(),
        }
    }

    pub(crate) fn badge_label(&self) -> &'static str {
        match self {
            Self::Global { .. } => "GLOBAL",
            Self::Local { .. } => "LAN",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub display_name: String,
    pub download_name: String,
    pub content_kind: &'static str,
    pub transport_label: String,
    pub input_size: u64,
    pub content_length: Option<u64>,
    pub expires_at: SystemTime,
    pub remaining: Duration,
    pub once: bool,
    pub requires_pin: bool,
    pub revealed_pin: Option<String>,
    pub primary_link_label: &'static str,
    pub primary_link: String,
    pub secondary_link_label: Option<&'static str>,
    pub secondary_link: Option<String>,
    pub provider_status: String,
    pub completed_downloads: u64,
    pub bytes_served: u64,
    pub active_download: bool,
    pub consumed: bool,
    pub warnings: Vec<String>,
    pub mode: ResolvedSendMode,
    pub token: String,
}

#[derive(Clone, Debug)]
struct MutableSessionState {
    mode: ResolvedSendMode,
    primary_link: String,
    secondary_link: Option<String>,
    provider_status: String,
    completed_downloads: u64,
    bytes_served: u64,
    active_download: bool,
    consumed: bool,
    warnings: Vec<String>,
}

pub struct SharedSession {
    token: String,
    content: ContentSource,
    access: AccessPolicy,
    revealed_pin: Option<String>,
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
        mode: ResolvedSendMode,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            token,
            content,
            access,
            revealed_pin,
            shutdown,
            state: Mutex::new(MutableSessionState {
                mode,
                primary_link: String::new(),
                secondary_link: None,
                provider_status: "Preparing session".to_string(),
                completed_downloads: 0,
                bytes_served: 0,
                active_download: false,
                consumed: false,
                warnings: Vec::new(),
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

    pub async fn set_links(&self, primary_link: String, secondary_link: Option<String>) {
        let mut state = self.state.lock().await;
        state.primary_link = primary_link;
        state.secondary_link = secondary_link;
    }

    pub async fn set_provider_status(&self, status: impl Into<String>) {
        let mut state = self.state.lock().await;
        state.provider_status = status.into();
    }

    pub async fn set_mode(&self, mode: ResolvedSendMode) {
        let mut state = self.state.lock().await;
        state.mode = mode;
    }

    pub async fn add_warning(&self, warning: impl Into<String>) {
        let warning = warning.into();
        let mut state = self.state.lock().await;
        if !state.warnings.iter().any(|existing| existing == &warning) {
            state.warnings.push(warning);
        }
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        let state = self.state.lock().await;
        let warnings = self
            .content
            .warnings()
            .iter()
            .cloned()
            .chain(state.warnings.iter().cloned())
            .collect::<Vec<_>>();
        SessionSnapshot {
            display_name: self.content.display_name().to_string(),
            download_name: self.content.download_name().to_string(),
            content_kind: self.content.kind_label(),
            transport_label: state.mode.transport_label(),
            input_size: self.content.input_size(),
            content_length: self.content.content_length(),
            expires_at: self.access.expires_at,
            remaining: remaining_until(self.access.expires_at),
            once: self.access.once,
            requires_pin: self.access.pin_hash.is_some(),
            revealed_pin: self.revealed_pin.clone(),
            primary_link_label: state.mode.primary_link_label(),
            primary_link: state.primary_link.clone(),
            secondary_link_label: state.mode.secondary_link_label(),
            secondary_link: state.secondary_link.clone(),
            provider_status: state.provider_status.clone(),
            completed_downloads: state.completed_downloads,
            bytes_served: state.bytes_served,
            active_download: state.active_download,
            consumed: state.consumed,
            warnings,
            mode: state.mode.clone(),
            token: self.token.clone(),
        }
    }

    async fn build_page(&self, supplied_token: &str) -> Result<String, SessionRejection> {
        self.validate_token(supplied_token)?;

        if self.is_expired() {
            return Err(SessionRejection::Expired);
        }

        let snapshot = self.snapshot().await;
        Ok(download_page::render(&snapshot))
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

        let state = self.state.lock().await;
        if state.consumed {
            return Err(SessionRejection::AlreadyConsumed);
        }

        Ok(())
    }

    async fn reserve_download(&self) -> Result<(), SessionRejection> {
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
    headers: HeaderMap,
) -> Response {
    if let Err(rejection) = session.begin_download(&token, query.pin.as_deref()).await {
        return rejection.into_response();
    }

    let range_header = headers
        .get(RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let planned = match plan_download(session.content(), range_header.as_deref()) {
        Ok(plan) => plan,
        Err(response) => return response,
    };

    if let Err(rejection) = session.reserve_download().await {
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
        let outcome = match planned.range {
            Some(range) => {
                content
                    .stream_range_to_channel(range.start, range.len(), tx, progress_tx)
                    .await
            }
            None => content.stream_to_channel(tx, progress_tx).await,
        };
        match outcome {
            Ok(_) => session_for_task.finish_download(true).await,
            Err(_) => session_for_task.finish_download(false).await,
        }
    });

    let mut response = Response::new(Body::from_stream(ReceiverStream::new(rx)));
    *response.status_mut() = planned.status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );

    if let Ok(value) =
        HeaderValue::from_str(&content_disposition(session.content().download_name()))
    {
        response.headers_mut().insert(CONTENT_DISPOSITION, value);
    }

    if let Ok(value) = HeaderValue::from_str(planned.accept_ranges) {
        response.headers_mut().insert(ACCEPT_RANGES, value);
    }

    if let Some(content_range) = planned.content_range {
        if let Ok(value) = HeaderValue::from_str(&content_range) {
            response.headers_mut().insert(CONTENT_RANGE, value);
        }
    }

    if let Some(length) = planned.content_length {
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

struct DownloadPlan {
    status: StatusCode,
    content_length: Option<u64>,
    content_range: Option<String>,
    accept_ranges: &'static str,
    range: Option<RequestedRange>,
}

#[derive(Clone, Copy)]
struct RequestedRange {
    start: u64,
    end: u64,
}

impl RequestedRange {
    fn len(self) -> u64 {
        self.end - self.start + 1
    }

    fn content_range(self, total: u64) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, total)
    }
}

fn plan_download(
    content: &ContentSource,
    range_header: Option<&str>,
) -> Result<DownloadPlan, Response> {
    if content.supports_range() {
        let total = content.content_length().unwrap_or(0);
        if let Some(range_header) = range_header {
            let range = parse_range_header(range_header, total).map_err(|_| {
                let mut response = (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "Invalid or unsatisfiable range",
                )
                    .into_response();
                if let Ok(value) = HeaderValue::from_str(&format!("bytes */{total}")) {
                    response.headers_mut().insert(CONTENT_RANGE, value);
                }
                response
            })?;
            return Ok(DownloadPlan {
                status: StatusCode::PARTIAL_CONTENT,
                content_length: Some(range.len()),
                content_range: Some(range.content_range(total)),
                accept_ranges: "bytes",
                range: Some(range),
            });
        }

        return Ok(DownloadPlan {
            status: StatusCode::OK,
            content_length: content.content_length(),
            content_range: None,
            accept_ranges: "bytes",
            range: None,
        });
    }

    Ok(DownloadPlan {
        status: StatusCode::OK,
        content_length: content.content_length(),
        content_range: None,
        accept_ranges: "none",
        range: None,
    })
}

fn parse_range_header(value: &str, total: u64) -> Result<RequestedRange, ()> {
    if total == 0 {
        return Err(());
    }

    let value = value.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') {
        return Err(());
    }

    let (start, end) = value.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix_len = end.parse::<u64>().map_err(|_| ())?;
        if suffix_len == 0 {
            return Err(());
        }
        let bounded = suffix_len.min(total);
        let range_start = total - bounded;
        return Ok(RequestedRange {
            start: range_start,
            end: total - 1,
        });
    }

    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= total {
        return Err(());
    }

    let end = if end.is_empty() {
        total - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(total - 1)
    };

    if end < start {
        return Err(());
    }

    Ok(RequestedRange { start, end })
}

#[cfg(test)]
mod tests {
    use super::{ResolvedSendMode, SharedSession, build_router};
    use crate::{
        access::build_access_policy,
        content::{ArchiveFormat, ContentSource},
        provider::ProviderKind,
    };
    use axum::http::{
        Request, StatusCode,
        header::{ACCEPT_RANGES, CONTENT_RANGE, RANGE},
    };
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
            ResolvedSendMode::Local {
                http_port: 8080,
                https_port: 8081,
            },
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
            ResolvedSendMode::Local {
                http_port: 8080,
                https_port: 8081,
            },
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

    #[tokio::test]
    async fn serves_partial_range_for_files() {
        let temp = tempdir().unwrap();
        let file = temp.path().join("hello.txt");
        fs::write(&file, b"beam-range").unwrap();
        let content = ContentSource::inspect(&file, ArchiveFormat::Zip).unwrap();
        let access = build_access_policy(Duration::from_secs(300), false, None);
        let session = SharedSession::new(
            "token123".to_string(),
            content,
            access.policy,
            access.revealed_pin,
            ResolvedSendMode::Local {
                http_port: 8080,
                https_port: 8081,
            },
            CancellationToken::new(),
        );
        let router = build_router(session);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/download/token123")
                    .header(RANGE, "bytes=5-9")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(CONTENT_RANGE).unwrap(),
            "bytes 5-9/10"
        );
        assert_eq!(response.headers().get(ACCEPT_RANGES).unwrap(), "bytes");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"range");
    }

    #[tokio::test]
    async fn rejects_unsatisfiable_ranges() {
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
            ResolvedSendMode::Local {
                http_port: 8080,
                https_port: 8081,
            },
            CancellationToken::new(),
        );
        let router = build_router(session);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/download/token123")
                    .header(RANGE, "bytes=50-99")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers().get(CONTENT_RANGE).unwrap(), "bytes */4");
    }

    #[tokio::test]
    async fn zip_streams_do_not_advertise_range_support() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("hello.txt"), b"beam").unwrap();
        let content = ContentSource::inspect(&folder, ArchiveFormat::Zip).unwrap();
        let access = build_access_policy(Duration::from_secs(300), false, None);
        let session = SharedSession::new(
            "token123".to_string(),
            content,
            access.policy,
            access.revealed_pin,
            ResolvedSendMode::Local {
                http_port: 8080,
                https_port: 8081,
            },
            CancellationToken::new(),
        );
        let router = build_router(session);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/download/token123")
                    .header(RANGE, "bytes=0-3")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(ACCEPT_RANGES).unwrap(), "none");
    }

    #[test]
    fn transport_labels_match_mode() {
        let global = ResolvedSendMode::Global {
            provider: ProviderKind::Cloudflared,
        };
        let local = ResolvedSendMode::Local {
            http_port: 8080,
            https_port: 8081,
        };

        assert_eq!(global.transport_label(), "HTTPS tunnel via cloudflared");
        assert_eq!(local.transport_label(), "HTTP primary + HTTPS optional");
    }
}
