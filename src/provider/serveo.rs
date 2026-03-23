use anyhow::{Context, Result, bail};
use regex::Regex;
use std::{
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    sync::{Mutex, mpsc, watch},
};
use url::Url;

use super::{TunnelHandle, TunnelRuntime, shutdown_child, ssh};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const HOST: &str = "serveo.net";
const PORT: u16 = 22;

pub(super) async fn start(local_url: &str) -> Result<TunnelHandle> {
    let origin = Url::parse(local_url).context("invalid local origin URL for Serveo")?;
    let host = origin
        .host_str()
        .context("Serveo requires a host in the local origin URL")?;
    let port = origin
        .port_or_known_default()
        .context("Serveo requires a port in the local origin URL")?;

    let mut command = ssh::ssh_tunnel_command(false);
    command
        .arg("-o")
        .arg("PreferredAuthentications=keyboard-interactive")
        .arg("-o")
        .arg("KbdInteractiveAuthentication=yes")
        .arg("-o")
        .arg("PubkeyAuthentication=no")
        .arg("-o")
        .arg("IdentityAgent=none")
        .arg("-p")
        .arg(PORT.to_string())
        .arg("-R")
        .arg(format!("80:{host}:{port}"))
        .arg(HOST);

    let mut child = command
        .spawn()
        .context("failed to spawn ssh for Serveo. Install OpenSSH and ensure it is on PATH")?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let (status_tx, status_rx) = watch::channel("Starting Serveo tunnel over SSH".to_string());
    let (url_tx, mut url_rx) = mpsc::unbounded_channel();
    let url_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let startup_error = Arc::new(StdMutex::new(None::<String>));
    let mut tasks = Vec::new();

    if let Some(stdout) = stdout {
        tasks.push(tokio::spawn(watch_serveo_output(
            BufReader::new(stdout),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
            startup_error.clone(),
        )));
    }

    if let Some(stderr) = stderr {
        tasks.push(tokio::spawn(watch_serveo_output(
            BufReader::new(stderr),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
            startup_error.clone(),
        )));
    }

    drop(url_tx);

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let public_url = loop {
        if let Ok(url) = url_rx.try_recv() {
            break url;
        }

        {
            let mut child = child.lock().await;
            if let Some(status) = child.try_wait().context("failed to query Serveo ssh status")? {
                let detail = startup_error
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .unwrap_or_else(|| format!("Serveo ssh exited before returning a public URL ({status})"));
                bail!("{detail}");
            }
        }

        if Instant::now() >= deadline {
            shutdown_child(child.clone()).await;
            let detail = startup_error
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| "timed out waiting for Serveo to expose a public URL".to_string());
            bail!("{detail}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let _ = status_tx.send(format!("Serveo ready at {public_url}"));

    Ok(TunnelHandle {
        public_url,
        provider_name: "serveo",
        runtime: TunnelRuntime::ExternalProcess { child },
        status_rx,
        tasks,
    })
}

pub(crate) async fn watch_serveo_output<R>(
    mut reader: BufReader<R>,
    status_tx: watch::Sender<String>,
    url_tx: mpsc::UnboundedSender<String>,
    url_sent: Arc<std::sync::atomic::AtomicBool>,
    startup_error: Arc<StdMutex<Option<String>>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    let regex = Regex::new(
        r"https://[A-Za-z0-9._-]+(?:\.[A-Za-z0-9._-]+)*\.(?:serveo\.net|serveousercontent\.com)",
    )
    .unwrap();
    let mut line = String::new();

    loop {
        line.clear();
        let read = match reader.read_line(&mut line).await {
            Ok(read) => read,
            Err(error) => {
                let _ = status_tx.send(format!("Serveo ssh read error: {error}"));
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

        let status = ssh::strip_ansi(trimmed).chars().take(120).collect::<String>();
        let _ = status_tx.send(status);

        ssh::record_ssh_provider_startup_error(&startup_error, trimmed);

        if !url_sent.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(url) = extract_serveo_public_url(trimmed, &regex) {
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

pub(crate) fn extract_serveo_public_url(line: &str, regex: &Regex) -> Option<String> {
    let clean_line = ssh::strip_ansi(line);
    regex.find_iter(&clean_line).find_map(|capture| {
        let url = capture.as_str();
        let parsed = Url::parse(url).ok()?;
        let host = parsed.host_str()?;
        if host.ends_with(".serveo.net") || host.ends_with(".serveousercontent.com") {
            Some(url.to_string())
        } else {
            None
        }
    })
}
