use std::{
    process::Stdio,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::process::Command;

pub(super) fn available() -> bool {
    super::command_exists(&ssh_command(), &["-V"])
}

pub(super) fn ssh_tunnel_command(batch_mode: bool) -> Command {
    let mut command = Command::new(ssh_command());
    command
        .arg("-T")
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
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if batch_mode {
        command.arg("-o").arg("BatchMode=yes");
    }
    command
}

pub(crate) fn seems_ssh_provider_error(line: &str) -> bool {
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

pub(crate) fn record_ssh_provider_startup_error(
    startup_error: &Arc<StdMutex<Option<String>>>,
    line: &str,
) {
    if !seems_ssh_provider_error(line) {
        return;
    }

    if let Ok(mut slot) = startup_error.lock() {
        *slot = Some(line.to_string());
    }
}

pub(crate) fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            while let Some(next) = chars.next() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }

        output.push(ch);
    }

    output
}

fn ssh_command() -> String {
    std::env::var("BEAM_SSH_BIN").unwrap_or_else(|_| "ssh".to_string())
}
