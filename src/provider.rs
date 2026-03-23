mod cloudflared;
mod native;
mod pinggy;
mod serveo;
mod ssh;

use anyhow::{Result, bail};
use async_trait::async_trait;
use clap::ValueEnum;
use std::{
    process::{Command as StdCommand, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Child,
    sync::{Mutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ProviderKind {
    #[default]
    Auto,
    Cloudflared,
    Pinggy,
    Serveo,
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
            ProviderKind::Serveo => start_explicit_provider(ProviderKind::Serveo, local_url).await,
            ProviderKind::Native => start_explicit_provider(ProviderKind::Native, local_url).await,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            ProviderKind::Auto => "auto",
            ProviderKind::Cloudflared => "cloudflared",
            ProviderKind::Pinggy => "pinggy",
            ProviderKind::Serveo => "serveo",
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
            Self::Serveo => "HTTPS tunnel via Serveo SSH",
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
    tasks: Vec<JoinHandle<()>>,
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
    cloudflared::available()
}

pub fn ssh_available() -> bool {
    ssh::available()
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
    pinggy::free_ttl_limit()
}

pub fn native_relay_configured() -> bool {
    native::relay_configured()
}

pub async fn native_available_for_auto() -> bool {
    native::available_for_auto().await
}

async fn start_explicit_provider(provider: ProviderKind, local_url: &str) -> Result<StartedTunnel> {
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
        ProviderKind::Pinggy | ProviderKind::Serveo => {
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
        ProviderKind::Cloudflared => cloudflared::start(local_url).await,
        ProviderKind::Pinggy => pinggy::start(local_url).await,
        ProviderKind::Serveo => serveo::start(local_url).await,
        ProviderKind::Native => native::start(local_url).await,
        ProviderKind::Auto => unreachable!("auto should be resolved before starting a provider"),
    }
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
        "\nHints: install cloudflared, ensure ssh is available for Pinggy, try --provider serveo for another SSH tunnel option, set BEAM_RELAY_URL or run beam-relay, or use --local.",
    );
    message
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
        ProviderKind, TunnelProvider, auto_provider_order, cloudflared, format_auto_startup_error,
        pinggy, serveo,
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
        write_executable(&path, &format!("#!/bin/sh\nset -eu\n{body}\n"));
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
        let line =
            "INF | Your quick Tunnel has been created! Visit it at https://beam-alpha.trycloudflare.com";
        assert_eq!(
            cloudflared::extract_public_url(line, &regex).as_deref(),
            Some("https://beam-alpha.trycloudflare.com")
        );
    }

    #[test]
    fn parses_public_pinggy_url() {
        let regex =
            Regex::new(r"https://[A-Za-z0-9._-]+(?:\.[A-Za-z0-9._-]+)*\.pinggy\.(?:link|io)")
                .unwrap();
        let line =
            "You are not authenticated. Upgrade at https://dashboard.pinggy.io\nhttps://qvlow-79-117-198-230.a.free.pinggy.link";
        assert_eq!(
            pinggy::extract_pinggy_public_url(line, &regex).as_deref(),
            Some("https://qvlow-79-117-198-230.a.free.pinggy.link")
        );
    }

    #[test]
    fn parses_public_serveo_url() {
        let regex = Regex::new(
            r"https://[A-Za-z0-9._-]+(?:\.[A-Za-z0-9._-]+)*\.(?:serveo\.net|serveousercontent\.com)",
        )
        .unwrap();
        let line =
            "\u{1b}[32mForwarding HTTP traffic from https://beam-preview.serveousercontent.com";
        assert_eq!(
            serveo::extract_serveo_public_url(line, &regex).as_deref(),
            Some("https://beam-preview.serveousercontent.com")
        );
    }

    #[test]
    fn identifies_cloudflared_error_lines() {
        assert!(cloudflared::seems_cloudflared_error(
            "2026-03-23T10:50:38Z ERR Error unmarshaling QuickTunnel response"
        ));
        assert!(cloudflared::seems_cloudflared_error(
            "failed to unmarshal quick Tunnel: invalid character 'e' looking for beginning of value"
        ));
        assert!(!cloudflared::seems_cloudflared_error(
            "2026-03-23T10:50:38Z INF Requesting new quick Tunnel on trycloudflare.com..."
        ));
    }

    #[test]
    fn prefers_rate_limit_error_details() {
        let error = Arc::new(StdMutex::new(None));

        cloudflared::record_cloudflared_startup_error(
            &error,
            "failed to unmarshal quick Tunnel: invalid character 'e' looking for beginning of value",
        );
        cloudflared::record_cloudflared_startup_error(
            &error,
            "2026-03-23T10:50:38Z ERR Error unmarshaling QuickTunnel response: error code: 1015 error=\"invalid character 'e' looking for beginning of value\" status_code=\"429 Too Many Requests\"",
        );

        let stored = error.lock().unwrap().clone().unwrap();
        assert!(stored.contains("429 Too Many Requests"));
        assert_eq!(cloudflared::cloudflared_error_priority(&stored), 3);
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

        let started = ProviderKind::Auto.start("http://127.0.0.1:3000").await.unwrap();

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
    async fn explicit_serveo_uses_ssh_tunnel() {
        let _guard = env_lock().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = TempDir::new().unwrap();
        let args_file = dir.path().join("serveo-args.txt");
        let ssh = write_script(
            &dir,
            "ssh",
            &format!(
                "printf '%s\n' \"$@\" > '{}'\necho '\033[32mForwarding HTTP traffic from https://beam-preview.serveousercontent.com'\nexec sleep 1",
                args_file.display()
            ),
        );
        let previous_ssh = std::env::var_os("BEAM_SSH_BIN");

        unsafe {
            std::env::set_var("BEAM_SSH_BIN", &ssh);
        }

        let started = ProviderKind::Serveo.start("http://127.0.0.1:3000").await.unwrap();

        restore_env_var("BEAM_SSH_BIN", previous_ssh);

        assert_eq!(started.provider, ProviderKind::Serveo);
        assert_eq!(
            started.handle.public_url,
            "https://beam-preview.serveousercontent.com"
        );
        started.handle.shutdown().await;

        let args = fs::read_to_string(args_file).unwrap();
        assert!(args.contains("PreferredAuthentications=keyboard-interactive"));
        assert!(args.contains("KbdInteractiveAuthentication=yes"));
        assert!(args.contains("PubkeyAuthentication=no"));
        assert!(args.contains("IdentityAgent=none"));
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
