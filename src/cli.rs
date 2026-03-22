use crate::{
    access::{GENERATE_PIN_SENTINEL, build_access_policy, generate_token},
    content::{ArchiveFormat, ContentSource},
    doctor,
    duration::{format_duration, parse_duration_value},
    provider::{ProviderKind, TunnelProvider},
    session::{SessionMode, SharedSession, build_router},
    tls, ui,
};
use anyhow::{Context, Result, bail};
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
use tokio::net::TcpListener;
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
    #[arg(long)]
    pub global: bool,
    #[arg(long, value_enum, default_value_t = ProviderKind::Cloudflared)]
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
    let content = ContentSource::inspect(&command.path, command.archive)?;
    let access = build_access_policy(command.ttl, command.once, command.pin.clone());
    let token = generate_token();
    let shutdown = CancellationToken::new();
    let session = SharedSession::new(
        token.clone(),
        content.clone(),
        access.policy.clone(),
        access.revealed_pin.clone(),
        if command.global {
            SessionMode::Global
        } else {
            SessionMode::Local
        },
        shutdown.clone(),
    );

    let router = build_router(session.clone());
    let local_host = detect_lan_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let (public_link, tunnel, tunnel_status_task, server_task, tls_shutdown_task) =
        if command.global {
            let listener =
                TcpListener::bind((IpAddr::V4(Ipv4Addr::LOCALHOST), command.port.unwrap_or(0)))
                    .await
                    .context("failed to bind Beam server port")?;
            let local_addr = listener.local_addr()?;
            let server_shutdown = shutdown.clone();
            let server_task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(server_shutdown.cancelled_owned())
                    .await
                    .map_err(anyhow::Error::from)
            });

            let local_origin = format!("http://127.0.0.1:{}", local_addr.port());
            session
                .set_provider_status("Starting public tunnel".to_string())
                .await;
            let handle = command
                .provider
                .start(&local_origin)
                .await
                .with_context(|| format!("unable to start {}", command.provider.name()))?;
            let public_link = format!("{}/?token={token}", handle.public_url.trim_end_matches('/'));
            let status_task = forward_tunnel_status(handle.subscribe_status(), session.clone());
            (
                public_link,
                Some(handle),
                Some(status_task),
                server_task,
                None,
            )
        } else {
            let tls_config = tls::build_local_tls_config(local_host)
                .await
                .context("failed to create Beam local HTTPS certificate")?;
            let listener = bind_std_listener(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                command.port.unwrap_or(0),
            ))?;
            let local_addr = listener.local_addr()?;
            let server_handle = Handle::new();
            let server_handle_for_shutdown = server_handle.clone();
            let server_shutdown = shutdown.clone();

            let server_task = tokio::spawn(async move {
                axum_server::from_tcp_rustls(listener, tls_config)
                    .context("failed to prepare local HTTPS server")?
                    .handle(server_handle)
                    .serve(router.into_make_service())
                    .await
                    .map_err(anyhow::Error::from)
            });
            let tls_shutdown_task = tokio::spawn(async move {
                server_shutdown.cancelled().await;
                server_handle_for_shutdown.graceful_shutdown(Some(Duration::from_secs(2)));
            });

            session
                .set_provider_status("Local HTTPS server ready (temporary certificate)".to_string())
                .await;

            (
                format!("https://{local_host}:{}/?token={token}", local_addr.port()),
                None,
                None,
                server_task,
                Some(tls_shutdown_task),
            )
        };

    session.set_public_link(public_link.clone()).await;
    session
        .set_provider_status(if command.global {
            "Public tunnel ready".to_string()
        } else {
            "Share over the same network with local HTTPS".to_string()
        })
        .await;

    if let Some(command_name) = copy_to_clipboard(&public_link)? {
        session
            .set_provider_status(format!("Link copied to clipboard via {command_name}"))
            .await;
    }

    let ttl_remaining = access
        .policy
        .expires_at
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
    tokio::select! {
        _ = shutdown.cancelled() => {}
        _ = tokio::signal::ctrl_c() => {
            session.set_provider_status("Interrupted by user").await;
            shutdown.cancel();
        }
    }

    if let Some(handle) = tunnel {
        handle.shutdown().await;
    }

    if let Some(task) = tunnel_status_task {
        task.abort();
    }

    if let Some(task) = tls_shutdown_task {
        let _ = task.await;
    }

    let server_result = server_task.await.context("beam server task panicked")?;
    if let Err(error) = server_result {
        bail!("beam server stopped unexpectedly: {error}");
    }

    ui_task.await.context("beam UI task panicked")??;
    Ok(())
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
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while status_rx.changed().await.is_ok() {
            let status = status_rx.borrow().clone();
            debug!("tunnel status: {status}");
            session.set_provider_status(status).await;
        }
    })
}
