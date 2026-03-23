use crate::{
    relay::relay_response_header_allowed,
    relay_protocol::{
        ClientToRelayMessage, DEFAULT_RELAY_URL, RelaySessionCreateRequest,
        RelaySessionCreateResponse, RelayToClientMessage, encode_body_chunk, headers_to_pairs,
        pairs_to_headers,
    },
};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use axum::http::StatusCode;
use clap::ValueEnum;
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use reqwest::Client;
use std::{
    process::{Command as StdCommand, Stdio},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, watch},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use url::Url;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ProviderKind {
    #[default]
    Auto,
    Cloudflared,
    Native,
}

#[async_trait]
pub trait TunnelProvider: Send + Sync {
    async fn start(&self, local_url: &str) -> Result<TunnelHandle>;
    fn name(&self) -> &'static str;
}

#[async_trait]
impl TunnelProvider for ProviderKind {
    async fn start(&self, local_url: &str) -> Result<TunnelHandle> {
        match self.resolve() {
            ProviderKind::Auto => unreachable!("auto should always resolve to a concrete provider"),
            ProviderKind::Cloudflared => start_cloudflared(local_url).await,
            ProviderKind::Native => start_native(local_url).await,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            ProviderKind::Auto => "auto",
            ProviderKind::Cloudflared => "cloudflared",
            ProviderKind::Native => "native",
        }
    }
}

impl ProviderKind {
    pub fn resolve(self) -> Self {
        match self {
            Self::Auto => resolve_auto_provider(cloudflared_available()),
            resolved => resolved,
        }
    }

    pub fn transport_label(self) -> &'static str {
        match self {
            Self::Auto => "HTTPS tunnel",
            Self::Cloudflared => "HTTPS tunnel via cloudflared",
            Self::Native => "HTTPS relay via native client",
        }
    }
}

fn resolve_auto_provider(cloudflared_present: bool) -> ProviderKind {
    if cloudflared_present {
        ProviderKind::Cloudflared
    } else {
        ProviderKind::Native
    }
}

enum TunnelRuntime {
    ExternalProcess { child: Arc<Mutex<Child>> },
    Managed { shutdown: CancellationToken },
}

pub struct TunnelHandle {
    pub public_url: String,
    pub provider_name: &'static str,
    runtime: TunnelRuntime,
    status_rx: watch::Receiver<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl TunnelHandle {
    pub fn subscribe_status(&self) -> watch::Receiver<String> {
        self.status_rx.clone()
    }

    pub async fn shutdown(self) {
        match self.runtime {
            TunnelRuntime::ExternalProcess { child } => {
                let mut child = child.lock().await;
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            TunnelRuntime::Managed { shutdown } => shutdown.cancel(),
        }

        for task in self.tasks {
            let _ = task.await;
        }
    }
}

pub fn cloudflared_available() -> bool {
    command_exists("cloudflared", &["--version"])
}

async fn start_cloudflared(local_url: &str) -> Result<TunnelHandle> {
    let mut command = Command::new("cloudflared");
    command
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(local_url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("failed to spawn cloudflared. Install it and ensure it is on PATH")?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let (status_tx, status_rx) = watch::channel("Starting cloudflared tunnel".to_string());
    let (url_tx, mut url_rx) = mpsc::unbounded_channel();
    let url_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let startup_error = Arc::new(StdMutex::new(None::<String>));
    let mut tasks = Vec::new();

    if let Some(stdout) = stdout {
        tasks.push(tokio::spawn(watch_cloudflared_output(
            BufReader::new(stdout),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
            startup_error.clone(),
        )));
    }

    if let Some(stderr) = stderr {
        tasks.push(tokio::spawn(watch_cloudflared_output(
            BufReader::new(stderr),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
            startup_error.clone(),
        )));
    }

    drop(url_tx);

    let deadline = Instant::now() + Duration::from_secs(20);
    let public_url = loop {
        if let Ok(url) = url_rx.try_recv() {
            break url;
        }

        {
            let mut child = child.lock().await;
            if let Some(status) = child.try_wait().context("failed to query cloudflared status")? {
                let detail = startup_error
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .unwrap_or_else(|| {
                        format!("cloudflared exited before returning a public URL ({status})")
                    });
                bail!("{detail}");
            }
        }

        if Instant::now() >= deadline {
            shutdown_child(child.clone()).await;
            let detail = startup_error
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| "timed out waiting for cloudflared to expose a public URL".to_string());
            bail!("{detail}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let _ = status_tx.send(format!("cloudflared ready at {public_url}"));

    Ok(TunnelHandle {
        public_url,
        provider_name: ProviderKind::Cloudflared.name(),
        runtime: TunnelRuntime::ExternalProcess { child },
        status_rx,
        tasks,
    })
}

async fn start_native(local_url: &str) -> Result<TunnelHandle> {
    let relay_url = relay_base_url()?;
    let client = Client::builder()
        .use_rustls_tls()
        .build()
        .context("failed to create Beam native relay HTTP client")?;
    let create_url = relay_url.join("v1/sessions").context("invalid relay URL")?;
    let request = RelaySessionCreateRequest {
        download_name: "beam".to_string(),
        expires_at_unix: unix_now() + 3600,
    };
    let session = client
        .post(create_url)
        .json(&request)
        .send()
        .await
        .context("failed to reach the Beam native relay")?
        .error_for_status()
        .context("Beam native relay rejected the session")?
        .json::<RelaySessionCreateResponse>()
        .await
        .context("failed to decode the Beam native relay response")?;

    let (status_tx, status_rx) = watch::channel("Connecting to Beam native relay".to_string());
    let shutdown = CancellationToken::new();
    let (ws_stream, _) = connect_async(session.websocket_url.as_str())
        .await
        .context("failed to open the Beam native relay websocket")?;
    let (mut ws_writer, mut ws_reader) = ws_stream.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Message>();
    let request_client = Client::builder()
        .use_rustls_tls()
        .build()
        .context("failed to create Beam local proxy HTTP client")?;

    let writer_shutdown = shutdown.clone();
    let writer_status = status_tx.clone();
    let writer_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = writer_shutdown.cancelled() => {
                    let _ = ws_writer.send(Message::Close(None)).await;
                    break;
                }
                maybe_message = outbound_rx.recv() => {
                    let Some(message) = maybe_message else { break; };
                    if ws_writer.send(message).await.is_err() {
                        let _ = writer_status.send("Beam native relay writer stopped".to_string());
                        break;
                    }
                }
            }
        }
    });

    let reader_shutdown = shutdown.clone();
    let reader_status = status_tx.clone();
    let local_origin = local_url.to_string();
    let reader_task = tokio::spawn(async move {
        while let Some(message_result) = ws_reader.next().await {
            let message = match message_result {
                Ok(message) => message,
                Err(error) => {
                    let _ = reader_status.send(format!("Beam native relay disconnected: {error}"));
                    break;
                }
            };

            match message {
                Message::Text(text) => {
                    let decoded = match serde_json::from_str::<RelayToClientMessage>(&text) {
                        Ok(message) => message,
                        Err(error) => {
                            let _ = reader_status
                                .send(format!("Beam native relay protocol error: {error}"));
                            break;
                        }
                    };

                    match decoded {
                        RelayToClientMessage::RequestStart {
                            request_id,
                            method,
                            path,
                            query,
                            headers,
                        } => {
                            let outbound_tx = outbound_tx.clone();
                            let request_client = request_client.clone();
                            let local_origin = local_origin.clone();
                            let reader_status = reader_status.clone();
                            tokio::spawn(async move {
                                if let Err(error) = proxy_relay_request(
                                    request_client,
                                    outbound_tx,
                                    &local_origin,
                                    request_id,
                                    &method,
                                    &path,
                                    query.as_deref(),
                                    &headers,
                                )
                                .await
                                {
                                    let _ = reader_status.send(format!(
                                        "Beam native relay request {request_id} failed: {error}"
                                    ));
                                }
                            });
                        }
                        RelayToClientMessage::SessionClose { reason } => {
                            let _ =
                                reader_status.send(format!("Beam native relay closed: {reason}"));
                            break;
                        }
                    }
                }
                Message::Close(_) => {
                    let _ = reader_status.send("Beam native relay closed".to_string());
                    break;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Binary(_) | Message::Frame(_) => {}
            }
        }

        reader_shutdown.cancel();
    });

    let _ = status_tx.send(format!("Beam native relay ready at {}", session.public_url));

    Ok(TunnelHandle {
        public_url: session.public_url,
        provider_name: ProviderKind::Native.name(),
        runtime: TunnelRuntime::Managed { shutdown },
        status_rx,
        tasks: vec![writer_task, reader_task],
    })
}

async fn proxy_relay_request(
    client: Client,
    outbound_tx: mpsc::UnboundedSender<Message>,
    local_origin: &str,
    request_id: u64,
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &[crate::relay_protocol::HeaderPair],
) -> Result<()> {
    let request_url = if let Some(query) = query {
        format!("{local_origin}{path}?{query}")
    } else {
        format!("{local_origin}{path}")
    };
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("unsupported proxied method {method}"))?;
    let mut request = client.request(method, &request_url);

    for (name, value) in pairs_to_headers(headers).iter() {
        request = request.header(name, value);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            send_control_message(
                &outbound_tx,
                ClientToRelayMessage::ResponseError {
                    request_id,
                    status: StatusCode::BAD_GATEWAY.as_u16(),
                    message: format!("Beam could not reach its local server: {error}"),
                },
            )?;
            return Err(error.into());
        }
    };

    send_control_message(
        &outbound_tx,
        ClientToRelayMessage::ResponseStart {
            request_id,
            status: response.status().as_u16(),
            headers: headers_to_pairs(response.headers(), relay_response_header_allowed),
        },
    )?;

    let mut body_stream = response.bytes_stream();
    while let Some(chunk_result) = body_stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                outbound_tx
                    .send(Message::Binary(encode_body_chunk(request_id, chunk).into()))
                    .map_err(|_| anyhow!("relay websocket sender stopped"))?;
            }
            Err(error) => {
                send_control_message(
                    &outbound_tx,
                    ClientToRelayMessage::ResponseError {
                        request_id,
                        status: StatusCode::BAD_GATEWAY.as_u16(),
                        message: format!("Beam relay response stream failed: {error}"),
                    },
                )?;
                return Err(error.into());
            }
        }
    }

    send_control_message(
        &outbound_tx,
        ClientToRelayMessage::ResponseEnd { request_id },
    )?;
    Ok(())
}

fn send_control_message(
    outbound_tx: &mpsc::UnboundedSender<Message>,
    message: ClientToRelayMessage,
) -> Result<()> {
    outbound_tx
        .send(Message::Text(
            serde_json::to_string(&message)
                .context("failed to serialize native relay control message")?
                .into(),
        ))
        .map_err(|_| anyhow!("relay websocket sender stopped"))
}

async fn watch_cloudflared_output<R>(
    mut reader: BufReader<R>,
    status_tx: watch::Sender<String>,
    url_tx: mpsc::UnboundedSender<String>,
    url_sent: Arc<std::sync::atomic::AtomicBool>,
    startup_error: Arc<StdMutex<Option<String>>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let regex = Regex::new(r"https://[A-Za-z0-9._/-]*trycloudflare\.com").unwrap();
    let mut line = String::new();

    loop {
        line.clear();
        let read = match reader.read_line(&mut line).await {
            Ok(read) => read,
            Err(error) => {
                let _ = status_tx.send(format!("cloudflared read error: {error}"));
                break;
            }
        };

        if read == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let status = trimmed.chars().take(120).collect::<String>();
        let _ = status_tx.send(status);

        record_cloudflared_startup_error(&startup_error, trimmed);

        if !url_sent.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(url) = extract_public_url(trimmed, &regex) {
                if url_sent
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .is_ok()
                {
                    let _ = url_tx.send(url);
                }
            }
        }
    }
}

fn seems_cloudflared_error(line: &str) -> bool {
    line.contains(" ERR ")
        || line.starts_with("ERR ")
        || line.starts_with("failed ")
        || line.starts_with("failed to ")
}

fn cloudflared_error_priority(line: &str) -> u8 {
    if line.contains("status_code=") || line.contains("Too Many Requests") {
        3
    } else if line.contains(" ERR ") || line.starts_with("ERR ") {
        2
    } else if seems_cloudflared_error(line) {
        1
    } else {
        0
    }
}

fn record_cloudflared_startup_error(
    startup_error: &Arc<StdMutex<Option<String>>>,
    line: &str,
) {
    if !seems_cloudflared_error(line) {
        return;
    }

    if let Ok(mut slot) = startup_error.lock() {
        match slot.as_ref() {
            None => *slot = Some(line.to_string()),
            Some(current)
                if cloudflared_error_priority(line) > cloudflared_error_priority(current) =>
            {
                *slot = Some(line.to_string())
            }
            _ => {}
        }
    }
}

fn extract_public_url(line: &str, regex: &Regex) -> Option<String> {
    regex.find(line).map(|capture| capture.as_str().to_string())
}

fn relay_base_url() -> Result<Url> {
    Url::parse(&std::env::var("BEAM_RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string()))
        .context("invalid BEAM_RELAY_URL")
}

fn command_exists(command: &str, args: &[&str]) -> bool {
    StdCommand::new(command)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

async fn shutdown_child(child: Arc<Mutex<Child>>) {
    let mut child = child.lock().await;
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderKind, TunnelProvider, cloudflared_error_priority, extract_public_url,
        record_cloudflared_startup_error, resolve_auto_provider, seems_cloudflared_error,
    };
    use crate::relay::RelayState;
    use axum::{Router, routing::get};
    use regex::Regex;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::net::TcpListener;
    use url::Url;

    #[test]
    fn parses_public_cloudflared_url() {
        let regex = Regex::new(r"https://[A-Za-z0-9._/-]*trycloudflare\.com").unwrap();
        let line = "INF | Your quick Tunnel has been created! Visit it at https://beam-alpha.trycloudflare.com";
        assert_eq!(
            extract_public_url(line, &regex).as_deref(),
            Some("https://beam-alpha.trycloudflare.com")
        );
    }

    #[test]
    fn resolves_auto_provider() {
        assert_eq!(resolve_auto_provider(true), ProviderKind::Cloudflared);
        assert_eq!(resolve_auto_provider(false), ProviderKind::Native);
        assert_eq!(
            ProviderKind::Cloudflared.resolve(),
            ProviderKind::Cloudflared
        );
        assert_eq!(ProviderKind::Native.resolve(), ProviderKind::Native);
    }

    #[test]
    fn identifies_cloudflared_error_lines() {
        assert!(seems_cloudflared_error(
            "2026-03-23T10:50:38Z ERR Error unmarshaling QuickTunnel response"
        ));
        assert!(seems_cloudflared_error(
            "failed to unmarshal quick Tunnel: invalid character 'e' looking for beginning of value"
        ));
        assert!(!seems_cloudflared_error(
            "2026-03-23T10:50:38Z INF Requesting new quick Tunnel on trycloudflare.com..."
        ));
    }

    #[test]
    fn prefers_rate_limit_error_details() {
        let error = Arc::new(StdMutex::new(None));

        record_cloudflared_startup_error(
            &error,
            "failed to unmarshal quick Tunnel: invalid character 'e' looking for beginning of value",
        );
        record_cloudflared_startup_error(
            &error,
            "2026-03-23T10:50:38Z ERR Error unmarshaling QuickTunnel response: error code: 1015 error=\"invalid character 'e' looking for beginning of value\" status_code=\"429 Too Many Requests\"",
        );

        let stored = error.lock().unwrap().clone().unwrap();
        assert!(stored.contains("429 Too Many Requests"));
        assert_eq!(cloudflared_error_priority(&stored), 3);
    }

    #[tokio::test]
    async fn native_provider_relays_http_get() {
        let origin_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let origin_addr = origin_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                origin_listener,
                Router::new().route("/", get(|| async { "beam-native" })),
            )
            .await
            .unwrap();
        });

        let relay_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        let relay_url = format!("http://{relay_addr}/");
        let relay_router = RelayState::router(Url::parse(&relay_url).unwrap());
        tokio::spawn(async move {
            axum::serve(relay_listener, relay_router).await.unwrap();
        });

        unsafe {
            std::env::set_var("BEAM_RELAY_URL", relay_url.clone());
        }

        let handle = ProviderKind::Native
            .start(&format!("http://{origin_addr}"))
            .await
            .unwrap();
        let response = reqwest::get(&handle.public_url).await.unwrap();
        let body = response.bytes().await.unwrap();
        assert_eq!(&body[..], b"beam-native");
        handle.shutdown().await;
    }
}
