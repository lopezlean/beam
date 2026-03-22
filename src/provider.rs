use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use clap::ValueEnum;
use regex::Regex;
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, watch},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ProviderKind {
    #[default]
    Cloudflared,
}

#[async_trait]
pub trait TunnelProvider: Send + Sync {
    async fn start(&self, local_url: &str) -> Result<TunnelHandle>;
    fn name(&self) -> &'static str;
}

#[async_trait]
impl TunnelProvider for ProviderKind {
    async fn start(&self, local_url: &str) -> Result<TunnelHandle> {
        match self {
            ProviderKind::Cloudflared => start_cloudflared(local_url).await,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            ProviderKind::Cloudflared => "cloudflared",
        }
    }
}

pub struct TunnelHandle {
    pub public_url: String,
    child: Arc<Mutex<Child>>,
    status_rx: watch::Receiver<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl TunnelHandle {
    pub fn subscribe_status(&self) -> watch::Receiver<String> {
        self.status_rx.clone()
    }

    pub async fn shutdown(self) {
        {
            let mut child = self.child.lock().await;
            let _ = child.start_kill();
            let _ = child.wait().await;
        }

        for task in self.tasks {
            let _ = task.await;
        }
    }
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
    let mut tasks = Vec::new();

    if let Some(stdout) = stdout {
        tasks.push(tokio::spawn(watch_output(
            BufReader::new(stdout),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
        )));
    }

    if let Some(stderr) = stderr {
        tasks.push(tokio::spawn(watch_output(
            BufReader::new(stderr),
            status_tx.clone(),
            url_tx.clone(),
            url_sent.clone(),
        )));
    }

    let public_url = match tokio::time::timeout(Duration::from_secs(20), url_rx.recv()).await {
        Ok(Some(url)) => url,
        Ok(None) => {
            shutdown_child(child.clone()).await;
            bail!("cloudflared exited before returning a public URL")
        }
        Err(_) => {
            shutdown_child(child.clone()).await;
            bail!("timed out waiting for cloudflared to expose a public URL")
        }
    };

    let _ = status_tx.send(format!("cloudflared ready at {public_url}"));

    Ok(TunnelHandle {
        public_url,
        child,
        status_rx,
        tasks,
    })
}

async fn watch_output<R>(
    mut reader: BufReader<R>,
    status_tx: watch::Sender<String>,
    url_tx: mpsc::UnboundedSender<String>,
    url_sent: Arc<std::sync::atomic::AtomicBool>,
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

fn extract_public_url(line: &str, regex: &Regex) -> Option<String> {
    regex.find(line).map(|capture| capture.as_str().to_string())
}

async fn shutdown_child(child: Arc<Mutex<Child>>) {
    let mut child = child.lock().await;
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::extract_public_url;
    use regex::Regex;

    #[test]
    fn parses_public_cloudflared_url() {
        let regex = Regex::new(r"https://[A-Za-z0-9._/-]*trycloudflare\.com").unwrap();
        let line = "INF | Your quick Tunnel has been created! Visit it at https://beam-alpha.trycloudflare.com";
        assert_eq!(
            extract_public_url(line, &regex).as_deref(),
            Some("https://beam-alpha.trycloudflare.com")
        );
    }
}
