use anyhow::{Context, Result, bail};
use regex::Regex;
use std::{
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::{Mutex, mpsc, watch},
};

use super::{TunnelHandle, TunnelRuntime, shutdown_child};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) fn available() -> bool {
    super::command_exists(&cloudflared_command(), &["--version"])
}

pub(super) async fn start(local_url: &str) -> Result<TunnelHandle> {
    let mut command = Command::new(cloudflared_command());
    command
        .arg("tunnel")
        .arg("--no-autoupdate")
        .arg("--url")
        .arg(local_url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

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

    let deadline = Instant::now() + STARTUP_TIMEOUT;
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
        provider_name: "cloudflared",
        runtime: TunnelRuntime::ExternalProcess { child },
        status_rx,
        tasks,
    })
}

pub(crate) async fn watch_cloudflared_output<R>(
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

pub(crate) fn seems_cloudflared_error(line: &str) -> bool {
    line.contains(" ERR ")
        || line.starts_with("ERR ")
        || line.starts_with("failed ")
        || line.starts_with("failed to ")
}

pub(crate) fn cloudflared_error_priority(line: &str) -> u8 {
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

pub(crate) fn record_cloudflared_startup_error(
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

pub(crate) fn extract_public_url(line: &str, regex: &Regex) -> Option<String> {
    regex.find(line).map(|capture| capture.as_str().to_string())
}

fn cloudflared_command() -> String {
    std::env::var("BEAM_CLOUDFLARED_BIN").unwrap_or_else(|_| "cloudflared".to_string())
}
