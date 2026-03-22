use crate::{
    access::{AccessSetup, GENERATE_PIN_SENTINEL, build_access_policy, generate_token},
    content::{ArchiveFormat, ContentSource},
    doctor,
    duration::{format_duration, parse_duration_value},
    provider::{ProviderKind, TunnelHandle, TunnelProvider},
    session::{RequestedSendMode, ResolvedSendMode, SharedSession, build_router},
    tls, ui,
};
use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum_server::Handle;
use clap::{Args, CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use std::{
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener, UdpSocket},
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::debug;

#[derive(Parser, Debug)]
#[command(
    name = "beam",
    version,
    about = "Ephemeral terminal-first file sharing"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Send(SendCommand),
    Doctor,
    Completion {
        #[arg(value_enum)]
        shell: Shell,
    },
    Version,
}

#[derive(Args, Debug)]
pub struct SendCommand {
    pub path: PathBuf,
    #[arg(short = 't', long = "ttl", default_value = "30m", value_parser = parse_duration_value)]
    pub ttl: Duration,
    #[arg(long)]
    pub once: bool,
    #[arg(long, conflicts_with = "local")]
    pub global: bool,
    #[arg(long, conflicts_with = "global")]
    pub local: bool,
    #[arg(long, value_enum, default_value_t = ProviderKind::Auto)]
    pub provider: ProviderKind,
    #[arg(
        long,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = GENERATE_PIN_SENTINEL
    )]
    pub pin: Option<String>,
    #[arg(long, value_enum, default_value_t = ArchiveFormat::Zip)]
    pub archive: ArchiveFormat,
    #[arg(long)]
    pub port: Option<u16>,
}

impl SendCommand {
    fn requested_mode(&self) -> RequestedSendMode {
        if self.local {
            RequestedSendMode::Local {
                base_port: self.port,
            }
        } else if self.global {
            RequestedSendMode::Global {
                provider: self.provider,
            }
        } else {
            RequestedSendMode::Global {
                provider: self.provider,
            }
        }
    }
}

struct RuntimeHandles {
    primary_link: String,
    secondary_link: Option<String>,
    ready_status: String,
    tunnel: Option<TunnelHandle>,
    tunnel_status_task: Option<JoinHandle<()>>,
    server_tasks: Vec<JoinHandle<Result<()>>>,
    shutdown_tasks: Vec<JoinHandle<()>>,
    server_error_rx: mpsc::UnboundedReceiver<String>,
}

impl RuntimeHandles {
    async fn shutdown(mut self) -> Result<()> {
        if let Some(handle) = self.tunnel.take() {
            handle.shutdown().await;
        }

        if let Some(task) = self.tunnel_status_task.take() {
            task.abort();
        }

        for task in self.shutdown_tasks {
            let _ = task.await;
        }

        let mut first_error = None;
        for task in self.server_tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
                Ok(Err(_)) => {}
                Err(error) if first_error.is_none() => {
                    first_error = Some(anyhow!("beam server task panicked: {error}"));
                }
                Err(_) => {}
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }
}

struct LocalBindings {
    http_listener: StdTcpListener,
    https_listener: StdTcpListener,
    resolved_mode: ResolvedSendMode,
}

pub async fn run() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Commands::Send(command) => run_send(command).await,
        Commands::Doctor => doctor::run().await,
        Commands::Completion { shell } => {
            let mut command = Cli::command();
            generate(shell, &mut command, "beam", &mut std::io::stdout());
            Ok(())
        }
        Commands::Version => {
            println!("beam {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

async fn run_send(command: SendCommand) -> Result<()> {
    let requested_mode = command.requested_mode();
    let content = ContentSource::inspect(&command.path, command.archive)?;
    let access = build_access_policy(command.ttl, command.once, command.pin.clone());
    let token = generate_token();
    let shutdown = CancellationToken::new();
    let local_host = detect_lan_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));

    let (session, mut runtime) = match requested_mode {
        RequestedSendMode::Global { provider } => {
            start_global_runtime(&command, content, access, token, shutdown.clone(), provider)
                .await?
        }
        RequestedSendMode::Local { base_port } => {
            start_local_runtime(
                content,
                access,
                token,
                shutdown.clone(),
                local_host,
                base_port,
            )
            .await?
        }
    };

    session
        .set_links(runtime.primary_link.clone(), runtime.secondary_link.clone())
        .await;
    session
        .set_provider_status(runtime.ready_status.clone())
        .await;

    if let Some(command_name) = copy_to_clipboard(&runtime.primary_link)? {
        session
            .set_provider_status(format!("Link copied to clipboard via {command_name}"))
            .await;
    }

    let ttl_remaining = session
        .expires_at()
        .duration_since(SystemTime::now())
        .unwrap_or_else(|_| Duration::ZERO);
    let ttl_session = session.clone();
    let ttl_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(ttl_remaining).await;
        ttl_session
            .set_provider_status(format!(
                "TTL expired after {}",
                format_duration(ttl_remaining)
            ))
            .await;
        ttl_shutdown.cancel();
    });

    let ui_task = tokio::spawn(ui::render_loop(session.clone()));
    let mut server_failure = None;

    tokio::select! {
        _ = shutdown.cancelled() => {}
        _ = tokio::signal::ctrl_c() => {
            session.set_provider_status("Interrupted by user").await;
            shutdown.cancel();
        }
        Some(error) = runtime.server_error_rx.recv() => {
            session.set_provider_status(error.clone()).await;
            server_failure = Some(error);
            shutdown.cancel();
        }
    }

    let shutdown_result = runtime.shutdown().await;
    ui_task.await.context("beam UI task panicked")??;

    if let Some(error) = server_failure {
        if let Err(cleanup_error) = shutdown_result {
            bail!("{error}\ncleanup failed: {cleanup_error}");
        }
        bail!("{error}");
    }

    shutdown_result?;
    Ok(())
}

async fn start_global_runtime(
    command: &SendCommand,
    content: ContentSource,
    access: AccessSetup,
    token: String,
    shutdown: CancellationToken,
    provider: ProviderKind,
) -> Result<(Arc<SharedSession>, RuntimeHandles)> {
    let resolved_provider = provider.resolve();
    let resolved_mode = ResolvedSendMode::Global {
        provider: resolved_provider,
    };
    let session = SharedSession::new(
        token.clone(),
        content,
        access.policy,
        access.revealed_pin,
        resolved_mode,
        shutdown.clone(),
    );
    let router = build_router(session.clone());
    let (server_error_tx, server_error_rx) = mpsc::unbounded_channel();
    let listener = TcpListener::bind((IpAddr::V4(Ipv4Addr::LOCALHOST), command.port.unwrap_or(0)))
        .await
        .context("failed to bind Beam global server port")?;
    let local_addr = listener.local_addr()?;
    let server_task = spawn_http_server(
        listener,
        router,
        shutdown.clone(),
        server_error_tx,
        "global HTTP origin",
    );

    let local_origin = format!("http://127.0.0.1:{}", local_addr.port());
    session
        .set_provider_status(format!(
            "Starting global link via {}",
            resolved_provider.name()
        ))
        .await;

    let tunnel = match resolved_provider.start(&local_origin).await {
        Ok(handle) => handle,
        Err(error) => {
            shutdown.cancel();
            let _ = server_task.await;
            return Err(global_startup_error(command, resolved_provider, error));
        }
    };

    let public_link = format!("{}/?token={token}", tunnel.public_url.trim_end_matches('/'));
    let status_task = forward_tunnel_status(tunnel.subscribe_status(), session.clone());

    Ok((
        session,
        RuntimeHandles {
            primary_link: public_link,
            secondary_link: None,
            ready_status: format!("Public link ready via {}", tunnel.provider_name),
            tunnel: Some(tunnel),
            tunnel_status_task: Some(status_task),
            server_tasks: vec![server_task],
            shutdown_tasks: Vec::new(),
            server_error_rx,
        },
    ))
}

async fn start_local_runtime(
    content: ContentSource,
    access: AccessSetup,
    token: String,
    shutdown: CancellationToken,
    local_host: IpAddr,
    base_port: Option<u16>,
) -> Result<(Arc<SharedSession>, RuntimeHandles)> {
    let bindings = bind_local_listeners(base_port)?;
    let session = SharedSession::new(
        token.clone(),
        content,
        access.policy,
        access.revealed_pin,
        bindings.resolved_mode.clone(),
        shutdown.clone(),
    );
    let router = build_router(session.clone());
    let (server_error_tx, server_error_rx) = mpsc::unbounded_channel();
    let tls_config = tls::build_local_tls_config(local_host)
        .await
        .context("failed to create Beam local HTTPS certificate")?;

    let http_listener =
        TcpListener::from_std(bindings.http_listener).context("failed to prepare HTTP listener")?;
    let http_task = spawn_http_server(
        http_listener,
        router.clone(),
        shutdown.clone(),
        server_error_tx.clone(),
        "local HTTP",
    );

    let https_handle = Handle::new();
    let https_shutdown_handle = https_handle.clone();
    let https_shutdown = shutdown.clone();
    let https_task = spawn_https_server(
        bindings.https_listener,
        router,
        tls_config,
        shutdown.clone(),
        server_error_tx,
        https_handle,
        "local HTTPS",
    );
    let https_shutdown_task = tokio::spawn(async move {
        https_shutdown.cancelled().await;
        https_shutdown_handle.graceful_shutdown(Some(Duration::from_secs(2)));
    });

    let (http_port, https_port) = match bindings.resolved_mode {
        ResolvedSendMode::Local {
            http_port,
            https_port,
        } => (http_port, https_port),
        ResolvedSendMode::Global { .. } => unreachable!("local bindings always resolve to local"),
    };

    Ok((
        session,
        RuntimeHandles {
            primary_link: format!("http://{local_host}:{http_port}/?token={token}"),
            secondary_link: Some(format!("https://{local_host}:{https_port}/?token={token}")),
            ready_status: "Local HTTP + HTTPS ready".to_string(),
            tunnel: None,
            tunnel_status_task: None,
            server_tasks: vec![http_task, https_task],
            shutdown_tasks: vec![https_shutdown_task],
            server_error_rx,
        },
    ))
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "beam=info".into()),
        )
        .with_target(false)
        .try_init();
}

fn detect_lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn bind_local_listeners(base_port: Option<u16>) -> Result<LocalBindings> {
    let http_listener = bind_requested_listener(base_port, "HTTP")?;
    let http_port = http_listener.local_addr()?.port();
    let (https_listener, https_port) = bind_https_listener(http_port)?;

    Ok(LocalBindings {
        http_listener,
        https_listener,
        resolved_mode: ResolvedSendMode::Local {
            http_port,
            https_port,
        },
    })
}

fn bind_requested_listener(requested_port: Option<u16>, label: &str) -> Result<StdTcpListener> {
    let addr = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        requested_port.unwrap_or(0),
    );

    bind_std_listener(addr).with_context(|| match requested_port {
        Some(port) => format!("failed to bind Beam {label} port {port}"),
        None => format!("failed to bind Beam {label} port"),
    })
}

fn bind_https_listener(http_port: u16) -> Result<(StdTcpListener, u16)> {
    for candidate in preferred_https_ports(http_port) {
        if let Ok(listener) = bind_std_listener(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            candidate,
        )) {
            return Ok((listener, candidate));
        }
    }

    bail!("failed to find a free HTTPS port above {http_port}")
}

fn preferred_https_ports(http_port: u16) -> Vec<u16> {
    let mut ports = Vec::new();
    let http_port = u32::from(http_port);

    for candidate in (http_port + 1)..=(http_port + 10) {
        if let Ok(port) = u16::try_from(candidate) {
            ports.push(port);
        }
    }

    for candidate in (http_port + 11)..=u32::from(u16::MAX) {
        if let Ok(port) = u16::try_from(candidate) {
            ports.push(port);
        }
    }

    ports
}

fn spawn_http_server(
    listener: TcpListener,
    router: Router,
    shutdown: CancellationToken,
    server_error_tx: mpsc::UnboundedSender<String>,
    label: &'static str,
) -> JoinHandle<Result<()>> {
    let shutdown_on_error = shutdown.clone();
    tokio::spawn(async move {
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .map_err(anyhow::Error::from);
        if let Err(error) = &result {
            let message = format!("{label} server stopped unexpectedly: {error}");
            let _ = server_error_tx.send(message);
            shutdown_on_error.cancel();
        }
        result
    })
}

fn spawn_https_server(
    listener: StdTcpListener,
    router: Router,
    tls_config: axum_server::tls_rustls::RustlsConfig,
    shutdown: CancellationToken,
    server_error_tx: mpsc::UnboundedSender<String>,
    server_handle: Handle<SocketAddr>,
    label: &'static str,
) -> JoinHandle<Result<()>> {
    let shutdown_on_error = shutdown.clone();
    tokio::spawn(async move {
        let result = axum_server::from_tcp_rustls(listener, tls_config)
            .context("failed to prepare local HTTPS server")?
            .handle(server_handle)
            .serve(router.into_make_service())
            .await
            .map_err(anyhow::Error::from);
        if let Err(error) = &result {
            let message = format!("{label} server stopped unexpectedly: {error}");
            let _ = server_error_tx.send(message);
            shutdown_on_error.cancel();
        }
        result
    })
}

fn bind_std_listener(addr: SocketAddr) -> Result<StdTcpListener> {
    let listener = StdTcpListener::bind(addr).context("failed to bind Beam server port")?;
    listener
        .set_nonblocking(true)
        .context("failed to configure Beam server socket")?;
    Ok(listener)
}

fn copy_to_clipboard(text: &str) -> Result<Option<&'static str>> {
    if cfg!(target_os = "macos") {
        if try_pipe_to_command("pbcopy", &[], text)? {
            return Ok(Some("pbcopy"));
        }
    }

    if try_pipe_to_command("wl-copy", &[], text)? {
        return Ok(Some("wl-copy"));
    }

    if try_pipe_to_command("xclip", &["-selection", "clipboard"], text)? {
        return Ok(Some("xclip"));
    }

    Ok(None)
}

fn try_pipe_to_command(command: &str, args: &[&str], text: &str) -> Result<bool> {
    let mut child = match Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };

    if let Some(stdin) = &mut child.stdin {
        stdin.write_all(text.as_bytes())?;
    }

    Ok(child.wait()?.success())
}

fn forward_tunnel_status(
    mut status_rx: tokio::sync::watch::Receiver<String>,
    session: Arc<SharedSession>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while status_rx.changed().await.is_ok() {
            let status = status_rx.borrow().clone();
            debug!("tunnel status: {status}");
            session.set_provider_status(status).await;
        }
    })
}

fn global_startup_error(
    command: &SendCommand,
    provider: ProviderKind,
    error: anyhow::Error,
) -> anyhow::Error {
    match provider {
        ProviderKind::Cloudflared => anyhow!(
            "Beam needs cloudflared for global sharing. Install it with brew install cloudflared, switch to the native relay with --provider native, or run beam send {} --local.\n\nDetails: {error}",
            command.path.display(),
        ),
        ProviderKind::Native => anyhow!(
            "Beam could not reach the native relay for global sharing. Start beam-relay or set BEAM_RELAY_URL, or run beam send {} --local.\n\nDetails: {error}",
            command.path.display(),
        ),
        ProviderKind::Auto => anyhow!(
            "Beam could not start global sharing automatically for {}.\n\nDetails: {error}",
            command.path.display(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, preferred_https_ports};
    use clap::Parser;

    #[test]
    fn defaults_to_global_mode() {
        let cli = Cli::parse_from(["beam", "send", "video.mp4"]);
        let command = match cli.command {
            super::Commands::Send(command) => command,
            _ => panic!("expected send command"),
        };

        assert!(!command.local);
        assert!(!command.global);
        assert_eq!(command.provider, crate::provider::ProviderKind::Auto);
        assert!(matches!(
            command.requested_mode(),
            crate::session::RequestedSendMode::Global { .. }
        ));
    }

    #[test]
    fn keeps_https_search_near_http_port() {
        let ports = preferred_https_ports(8080);
        assert_eq!(
            &ports[..10],
            &[8081, 8082, 8083, 8084, 8085, 8086, 8087, 8088, 8089, 8090]
        );
        assert_eq!(ports[10], 8091);
    }

    #[test]
    fn local_and_global_flags_conflict() {
        assert!(Cli::try_parse_from(["beam", "send", "video.mp4", "--local", "--global"]).is_err());
    }
}
