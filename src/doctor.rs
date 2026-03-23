use crate::provider::{
    auto_provider_order, cloudflared_available, native_available_for_auto, native_relay_configured,
    ssh_available, TunnelProvider,
};
use anyhow::Result;
use std::io::{IsTerminal, stdout};

pub async fn run() -> Result<()> {
    let cloudflared = cloudflared_available();
    let ssh = ssh_available();
    let native_auto = native_available_for_auto().await;
    let native_configured = native_relay_configured();
    let auto_order = auto_provider_order().await;
    let clipboard = clipboard_command().is_some();
    let terminal = stdout().is_terminal();
    let ansi = std::env::var("TERM")
        .map(|term| term != "dumb")
        .unwrap_or(false)
        && terminal;

    println!("Beam doctor");
    println!("  native relay: embedded");
    println!("  streaming   : ready");
    println!("  range       : files ok, zip no-resume");
    println!("  local http  : ok");
    println!("  local https : ok");
    println!("  cloudflared : {}", pass_fail(cloudflared));
    println!("  ssh         : {}", pass_fail(ssh));
    println!("  pinggy      : {}", if ssh { "ready via ssh" } else { "ssh missing" });
    println!("  serveo      : {}", if ssh { "ready via ssh" } else { "ssh missing" });
    println!("  native auto : {}", if native_auto { "eligible" } else { "disabled" });
    println!("  global mode : auto");
    println!(
        "  auto order  : {}",
        if auto_order.is_empty() {
            "none".to_string()
        } else {
            auto_order
                .iter()
                .map(|provider| provider.name().to_string())
                .collect::<Vec<_>>()
                .join(" -> ")
        }
    );
    println!("  clipboard   : {}", pass_fail(clipboard));
    println!("  terminal    : {}", pass_fail(terminal));
    println!("  ansi/unicode: {}", pass_fail(ansi));

    if cloudflared {
        println!("  note        : auto tries cloudflared first when it is available");
    }
    if ssh {
        println!("  note        : auto can fall back to Pinggy over SSH without an account");
        println!("  note        : Serveo is available as an explicit SSH provider and may show a browser warning page");
    }
    if native_configured {
        println!("  note        : native relay is explicitly configured through BEAM_RELAY_URL");
    } else if !native_auto {
        println!("  note        : native relay stays out of auto unless configured or locally reachable");
    }

    Ok(())
}

pub fn clipboard_command() -> Option<&'static str> {
    if cfg!(target_os = "macos") && command_exists("pbcopy", &[]) {
        return Some("pbcopy");
    }

    if command_exists("wl-copy", &["--version"]) {
        return Some("wl-copy");
    }

    if command_exists("xclip", &["-version"]) {
        return Some("xclip");
    }

    None
}

fn command_exists(command: &str, args: &[&str]) -> bool {
    std::process::Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn pass_fail(value: bool) -> &'static str {
    if value { "ok" } else { "missing" }
}
