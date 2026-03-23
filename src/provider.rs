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

const CLOUDFLARED_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const PINGGY_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const PINGGY_HOST: &str = "free.pinggy.io";
const PINGGY_PORT: u16 = 443;
const PINGGY_FREE_TTL_LIMIT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ProviderKind {
    #[default]
    Auto,
    Cloudflared,
    Pinggy,
    Native,
}

pub struct StartedTunnel {
    pub provider: ProviderKind,
    pub handle: TunnelHandle,
    pub ready_status: String,
}

#[derive(Clone, Debug)]
struct ProviderAttempt {
    provider: ProviderKind,
    state: &'static str,
    detail: String,
}

#[async_trait]
pub trait TunnelProvider: Send + Sync {
    async fn start(&self, local_url: &str) -> Result<StartedTunnel>;
    fn name(&self) -> &'static str;
}

#[async_trait]
impl TunnelProvider for ProviderKind {
    async fn start(&self, local_url: &str) -> Result<StartedTunnel> {
        match self {
            ProviderKind::Auto => start_auto(local_url).await,
            ProviderKind::Cloudflared => {
                start_explicit_provider(ProviderKind::Cloudflared, local_url).await
            }
            ProviderKind::Pinggy => start_explicit_provider(ProviderKind::Pinggy, local_url).await,
            ProviderKind::Native => start_explicit_provider(ProviderKind::Native, local_url).await,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            ProviderKind::Auto => "auto",
            ProviderKind::Cloudflared => "cloudflared",
            ProviderKind::Pinggy => "pinggy",
            ProviderKind::Native => "native",
        }
    }
}

impl ProviderKind {
    pub fn transport_label(self) -> &'static str {
        match self {
            Self::Auto => "HTTPS tunnel",
            Self::Cloudflared => "HTTPS tunnel via cloudflared",
            Self::Pinggy => "HTTPS tunnel via Pinggy SSH",
            Self::Native => "HTTPS relay via native client",
        }
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
    command_exists(&cloudflared_command(), &["--version"])
}

pub fn ssh_available() -> bool {
    command_exists(&ssh_command(), &["-V"])
}

pub async fn auto_provider_order() -> Vec<ProviderKind> {
    let mut providers = Vec::new();

    if cloudflared_available() {
        providers.push(ProviderKind::Cloudflared);
    }

    if ssh_available() {
        providers.push(ProviderKind::Pinggy);
    }

    if native_available_for_auto().await {
        providers.push(ProviderKind::Native);
    }

    providers
}

pub fn pinggy_free_ttl_limit() -> Duration {
    PINGGY_FREE_TTL_LIMIT
}

pub fn native_relay_configured() -> bool {
    std::env::var_os("BEAM_RELAY_URL").is_some()
}

pub async fn native_available_for_auto() -> bool {
    native_relay_configured() || local_default_native_relay_reachable().await
}

async fn start_explicit_provider(
    provider: ProviderKind,
    local_url: &str,
) -> Result<StartedTunnel> {
    let handle = start_concrete_provider(provider, local_url).await?;
    Ok(StartedTunnel {
        provider,
        ready_status: format!("Public link ready via {}", handle.provider_name),
        handle,
    })
}

async fn start_auto(local_url: &str) -> Result<StartedTunnel> {
    let mut attempts = Vec::new();

    for provider in [ProviderKind::Cloudflared, ProviderKind::Pinggy, ProviderKind::Native] {
        match provider_auto_availability(provider).await {
            Ok(()) => match start_concrete_provider(provider, local_url).await {
                Ok(handle) => {
                    let ready_status = format_auto_ready_status(handle.provider_name, &attempts);
                    return Ok(StartedTunnel {
                        provider,
                        handle,
                        ready_status,
                    });
                }
                Err(error) => attempts.push(ProviderAttempt {
                    provider,
                    state: "failed",
                    detail: error.to_string(),
                }),
            },
            Err(reason) => attempts.push(ProviderAttempt {
                provider,
                state: "unavailable",
                detail: reason,
            }),
        }
    }

    bail!(format_auto_startup_error(&attempts));
}

async fn provider_auto_availability(provider: ProviderKind) -> std::result::Result<(), String> {
    match provider {
        ProviderKind::Cloudflared => {
            if cloudflared_available() {
                Ok(())
            } else {
                Err("cloudflared is not installed or not on PATH".to_string())
            }
        }
        ProviderKind::Pinggy => {
            if ssh_available() {
                Ok(())
            } else {
                Err("ssh is not installed or not on PATH".to_string())
            }
        }
        ProviderKind::Native => {
            if native_available_for_auto().await {
                Ok(())
            } else {
                Err(
                    "no BEAM_RELAY_URL is configured and the default local relay is not reachable"
                        .to_string(),
                )
            }
        }
        ProviderKind::Auto => unreachable!("auto availability should check concrete providers"),
    }
}

async fn start_concrete_provider(provider: ProviderKind, local_url: &str) -> Result<TunnelHandle> {
    match provider {
        ProviderKind::Cloudflared => start_cloudflared(local_url).await,
        ProviderKind::Pinggy => start_pinggy(local_url).await,
        ProviderKind::Native => start_native(local_url).await,
        ProviderKind::Auto => unreachable!("auto should be resolved before starting a provider"),
    }
}

async fn start_cloudflared(local_url: &str) -> Result<TunnelHandle> {
    let mut command = Command::new(cloudflared_command());
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

    let deadline = Instant::now() + CLOUDFLARED_STARTUP_TIMEOUT;
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

async fn start_pinggy(local_url: &str) -> Result<TunnelHandle> {
    let origin = Url::parse(local_url).context("invalid local origin URL for Pinggy")?;
    let host = origin
        .host_str()
        .context("Pinggy requires a host in the local origin URL")?;
    let port = origin
        .port_or_known_default()
        .context("Pinggy requires a port in the local origin URL")?;

    let mut command = Command::new(ssh_command());
    command
        .arg("-T")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .arg("-o")
        .arg("ServerAliveInterval=30")
        .arg("-p")
        .arg(PINGGY_PORT.to_string())
        .arg("-R")
        .arg(format!("0:{host}:{port}"))
        .arg(PINGGY_HOST)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("failed to spawn ssh for Pinggy. Install OpenSSH and ensure it is on PATH")?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let (status_tx, status_rx) = watch::channel("Starting Pinggy tunnel over SSH".to_string());
    let (url_tx, mut url_rx) = mpsc::unbounded_channel();
    let url_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let startup_error = Arc::new(StdMutex::new(None::<String>));
    let mut tasks = Vec::new();

    if let Some(stdout) = stdout {
        tasks.push(tokio::spawn(watch_pinggy_output(
            BufReader::new(stdout),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
            startup_error.clone(),
        )));
    }

    if let Some(stderr) = stderr {
        tasks.push(tokio::spawn(watch_pinggy_output(
            BufReader::new(stderr),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
            startup_error.clone(),
        )));
    }

    drop(url_tx);

    let deadline = Instant::now() + PINGGY_STARTUP_TIMEOUT;
    let public_url = loop {
        if let Ok(url) = url_rx.try_recv() {
            break url;
        }

        {
            let mut child = child.lock().await;
            if let Some(status) = child.try_wait().context("failed to query Pinggy ssh status")? {
                let detail = startup_error
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .unwrap_or_else(|| format!("Pinggy ssh exited before returning a public URL ({status})"));
                bail!("{detail}");
            }
        }

        if Instant::now() >= deadline {
            shutdown_child(child.clone()).await;
            let detail = startup_error
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| "timed out waiting for Pinggy to expose a public URL".to_string());
            bail!("{detail}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let _ = status_tx.send(format!("Pinggy ready at {public_url}"));

    Ok(TunnelHandle {
        public_url,
        provider_name: ProviderKind::Pinggy.name(),
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

async fn watch_pinggy_output<R>(
    mut reader: BufReader<R>,
    status_tx: watch::Sender<String>,
    url_tx: mpsc::UnboundedSender<String>,
    url_sent: Arc<std::sync::atomic::AtomicBool>,
    startup_error: Arc<StdMutex<Option<String>>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let regex = Regex::new(r"https://[A-Za-z0-9._-]+(?:\.[A-Za-z0-9._-]+)*\.pinggy\.(?:link|io)")
        .unwrap();
    let mut line = String::new();

    loop {
        line.clear();
        let read = match reader.read_line(&mut line).await {
            Ok(read) => read,
            Err(error) => {
                let _ = status_tx.send(format!("Pinggy ssh read error: {error}"));
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

        record_pinggy_startup_error(&startup_error, trimmed);

        if !url_sent.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(url) = extract_pinggy_public_url(trimmed, &regex) {
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

fn seems_pinggy_error(line: &str) -> bool {
    line.starts_with("ssh: ")
        || line.contains("Permission denied")
        || line.contains("Host key verification failed")
        || line.contains("Could not resolve hostname")
        || line.contains("Connection timed out")
        || line.contains("Connection refused")
        || line.contains("remote port forwarding failed")
        || line.contains("administratively prohibited")
        || line.contains("kex_exchange_identification")
}

fn record_pinggy_startup_error(startup_error: &Arc<StdMutex<Option<String>>>, line: &str) {
    if !seems_pinggy_error(line) {
        return;
    }

    if let Ok(mut slot) = startup_error.lock() {
        *slot = Some(line.to_string());
    }
}

fn extract_public_url(line: &str, regex: &Regex) -> Option<String> {
    regex.find(line).map(|capture| capture.as_str().to_string())
}

fn extract_pinggy_public_url(line: &str, regex: &Regex) -> Option<String> {
    regex.find_iter(line).find_map(|capture| {
        let url = capture.as_str();
        let parsed = Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        if host == "dashboard.pinggy.io" {
            return None;
        }

        if host.ends_with(".pinggy.link")
            || (host.ends_with(".pinggy.io") && !host.starts_with("dashboard."))
        {
            Some(url.to_string())
        } else {
            None
        }
    })
}

fn format_auto_ready_status(provider_name: &str, attempts: &[ProviderAttempt]) -> String {
    if attempts.is_empty() {
        return format!("Public link ready via {provider_name}");
    }

    let summary = attempts
        .iter()
        .map(ProviderAttempt::brief)
        .collect::<Vec<_>>()
        .join(", ");
    format!("Public link ready via {provider_name} after {summary}")
}

fn format_auto_startup_error(attempts: &[ProviderAttempt]) -> String {
    let mut message = String::from("Beam could not start global sharing automatically.\n\n");
    message.push_str("Auto provider order: cloudflared -> pinggy");
    if attempts.iter().any(|attempt| attempt.provider == ProviderKind::Native) {
        message.push_str(" -> native");
    }
    message.push_str("\n\nAttempts:\n");

    for attempt in attempts {
        message.push_str(&format!("- {}\n", attempt.full()));
    }

    message.push_str(
        "\nHints: install cloudflared, ensure ssh is available for Pinggy, set BEAM_RELAY_URL or run beam-relay, or use --local.",
    );
    message
}

fn cloudflared_command() -> String {
    std::env::var("BEAM_CLOUDFLARED_BIN").unwrap_or_else(|_| "cloudflared".to_string())
}

fn ssh_command() -> String {
    std::env::var("BEAM_SSH_BIN").unwrap_or_else(|_| "ssh".to_string())
}

fn relay_base_url() -> Result<Url> {
    Url::parse(&std::env::var("BEAM_RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string()))
        .context("invalid BEAM_RELAY_URL")
}

async fn local_default_native_relay_reachable() -> bool {
    let relay_url = match Url::parse(DEFAULT_RELAY_URL) {
        Ok(url) => url,
        Err(_) => return false,
    };

    let Some(host) = relay_url.host_str() else {
        return false;
    };

    if host != "127.0.0.1" && host != "localhost" {
        return false;
    }

    let health_url = match relay_url.join("healthz") {
        Ok(url) => url,
        Err(_) => return false,
    };

    let client = match Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_millis(400))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    matches!(
        client.get(health_url).send().await,
        Ok(response) if response.status().is_success()
    )
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

impl ProviderAttempt {
    fn brief(&self) -> String {
        format!("{} {}", self.provider.name(), self.state)
    }

    fn full(&self) -> String {
        format!("{} {}: {}", self.provider.name(), self.state, self.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderKind, TunnelProvider, auto_provider_order, cloudflared_error_priority,
        extract_pinggy_public_url, extract_public_url, format_auto_startup_error,
        record_cloudflared_startup_error, seems_cloudflared_error,
    };
    use crate::relay::RelayState;
    use axum::{Router, routing::get};
    use regex::Regex;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        sync::{Arc, Mutex as StdMutex, OnceLock},
    };
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use url::Url;

    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    fn write_script(dir: &TempDir, name: &str, body: &str) -> String {
        let path = dir.path().join(name);
        write_executable(
            &path,
            &format!("#!/bin/sh\nset -eu\n{body}\n"),
        );
        path.to_string_lossy().into_owned()
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn restore_env_var(name: &str, previous: Option<std::ffi::OsString>) {
        match previous {
            Some(value) => unsafe { std::env::set_var(name, value) },
            None => unsafe { std::env::remove_var(name) },
        }
    }

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
    fn parses_public_pinggy_url() {
        let regex =
            Regex::new(r"https://[A-Za-z0-9._-]+(?:\.[A-Za-z0-9._-]+)*\.pinggy\.(?:link|io)")
                .unwrap();
        let line = "You are not authenticated. Upgrade at https://dashboard.pinggy.io\nhttps://qvlow-79-117-198-230.a.free.pinggy.link";
        assert_eq!(
            extract_pinggy_public_url(line, &regex).as_deref(),
            Some("https://qvlow-79-117-198-230.a.free.pinggy.link")
        );
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
    async fn resolves_auto_order_from_available_providers() {
        let _guard = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let cloudflared = write_script(&dir, "cloudflared", "echo version >/dev/null");
        let ssh = write_script(&dir, "ssh", "echo OpenSSH >/dev/null");
        let previous_cloud = std::env::var_os("BEAM_CLOUDFLARED_BIN");
        let previous_ssh = std::env::var_os("BEAM_SSH_BIN");
        let previous_relay = std::env::var_os("BEAM_RELAY_URL");

        unsafe {
            std::env::set_var("BEAM_CLOUDFLARED_BIN", &cloudflared);
            std::env::set_var("BEAM_SSH_BIN", &ssh);
            std::env::remove_var("BEAM_RELAY_URL");
        }

        let order = auto_provider_order().await;

        restore_env_var("BEAM_CLOUDFLARED_BIN", previous_cloud);
        restore_env_var("BEAM_SSH_BIN", previous_ssh);
        restore_env_var("BEAM_RELAY_URL", previous_relay);

        assert_eq!(order, vec![ProviderKind::Cloudflared, ProviderKind::Pinggy]);
    }

    #[tokio::test]
    async fn auto_falls_back_to_pinggy_when_cloudflared_fails() {
        let _guard = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let cloudflared = write_script(
            &dir,
            "cloudflared",
            "echo '2026-03-23T10:50:38Z ERR Error unmarshaling QuickTunnel response: status_code=\"429 Too Many Requests\"' >&2\nexit 1",
        );
        let ssh = write_script(
            &dir,
            "ssh",
            "echo 'Allocated port 7 for remote forward to localhost:3000'\necho 'https://beam-test.a.free.pinggy.link'\nexec sleep 1",
        );
        let previous_cloud = std::env::var_os("BEAM_CLOUDFLARED_BIN");
        let previous_ssh = std::env::var_os("BEAM_SSH_BIN");
        let previous_relay = std::env::var_os("BEAM_RELAY_URL");

        unsafe {
            std::env::set_var("BEAM_CLOUDFLARED_BIN", &cloudflared);
            std::env::set_var("BEAM_SSH_BIN", &ssh);
            std::env::remove_var("BEAM_RELAY_URL");
        }

        let started = ProviderKind::Auto
            .start("http://127.0.0.1:3000")
            .await
            .unwrap();

        restore_env_var("BEAM_CLOUDFLARED_BIN", previous_cloud);
        restore_env_var("BEAM_SSH_BIN", previous_ssh);
        restore_env_var("BEAM_RELAY_URL", previous_relay);

        assert_eq!(started.provider, ProviderKind::Pinggy);
        assert_eq!(
            started.handle.public_url,
            "https://beam-test.a.free.pinggy.link"
        );
        assert!(started.ready_status.contains("cloudflared failed"));
        started.handle.shutdown().await;
    }

    #[tokio::test]
    async fn explicit_pinggy_does_not_fallback() {
        let _guard = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let cloudflared = write_script(
            &dir,
            "cloudflared",
            "echo 'https://beam-alpha.trycloudflare.com'\nexec sleep 1",
        );
        let ssh = write_script(
            &dir,
            "ssh",
            "echo 'ssh: Could not resolve hostname free.pinggy.io: Name or service not known' >&2\nexit 255",
        );
        let previous_cloud = std::env::var_os("BEAM_CLOUDFLARED_BIN");
        let previous_ssh = std::env::var_os("BEAM_SSH_BIN");

        unsafe {
            std::env::set_var("BEAM_CLOUDFLARED_BIN", &cloudflared);
            std::env::set_var("BEAM_SSH_BIN", &ssh);
        }

        let error = match ProviderKind::Pinggy.start("http://127.0.0.1:3000").await {
            Ok(started) => {
                started.handle.shutdown().await;
                panic!("expected Pinggy startup to fail");
            }
            Err(error) => error.to_string(),
        };

        restore_env_var("BEAM_CLOUDFLARED_BIN", previous_cloud);
        restore_env_var("BEAM_SSH_BIN", previous_ssh);

        assert!(error.contains("Could not resolve hostname"));
        assert!(!error.contains("cloudflared"));
    }

    #[tokio::test]
    async fn auto_reports_ordered_provider_failures() {
        let _guard = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let cloudflared = write_script(
            &dir,
            "cloudflared",
            "echo 'ERR quick tunnel failed' >&2\nexit 1",
        );
        let ssh = write_script(
            &dir,
            "ssh",
            "echo 'ssh: Connection timed out during banner exchange' >&2\nexit 255",
        );
        let previous_cloud = std::env::var_os("BEAM_CLOUDFLARED_BIN");
        let previous_ssh = std::env::var_os("BEAM_SSH_BIN");
        let previous_relay = std::env::var_os("BEAM_RELAY_URL");

        unsafe {
            std::env::set_var("BEAM_CLOUDFLARED_BIN", &cloudflared);
            std::env::set_var("BEAM_SSH_BIN", &ssh);
            std::env::remove_var("BEAM_RELAY_URL");
        }

        let error = match ProviderKind::Auto.start("http://127.0.0.1:3000").await {
            Ok(started) => {
                started.handle.shutdown().await;
                panic!("expected auto startup to fail");
            }
            Err(error) => error.to_string(),
        };

        restore_env_var("BEAM_CLOUDFLARED_BIN", previous_cloud);
        restore_env_var("BEAM_SSH_BIN", previous_ssh);
        restore_env_var("BEAM_RELAY_URL", previous_relay);

        assert!(error.contains("cloudflared failed"));
        assert!(error.contains("pinggy failed"));
    }

    #[test]
    fn formats_auto_startup_error_with_ordered_attempts() {
        let error = format_auto_startup_error(&[
            super::ProviderAttempt {
                provider: ProviderKind::Cloudflared,
                state: "failed",
                detail: "rate limited".to_string(),
            },
            super::ProviderAttempt {
                provider: ProviderKind::Pinggy,
                state: "unavailable",
                detail: "ssh missing".to_string(),
            },
        ]);

        assert!(error.contains("cloudflared failed: rate limited"));
        assert!(error.contains("pinggy unavailable: ssh missing"));
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

        let started = ProviderKind::Native
            .start(&format!("http://{origin_addr}"))
            .await
            .unwrap();
        let response = reqwest::get(&started.handle.public_url).await.unwrap();
        let body = response.bytes().await.unwrap();
        assert_eq!(&body[..], b"beam-native");
        started.handle.shutdown().await;
    }
}
