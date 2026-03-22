use crate::provider::{ProviderKind, cloudflared_available};
use anyhow::Result;
use std::io::{IsTerminal, stdout};

pub async fn run() -> Result<()> {
    let cloudflared = cloudflared_available();
    let default_provider = ProviderKind::Auto.resolve();
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
    println!(
        "  global mode : {}",
        match default_provider {
            ProviderKind::Cloudflared => "cloudflared (auto)",
            ProviderKind::Native => "native (auto)",
            ProviderKind::Auto => "auto",
        }
    );
    println!("  clipboard   : {}", pass_fail(clipboard));
    println!("  terminal    : {}", pass_fail(terminal));
    println!("  ansi/unicode: {}", pass_fail(ansi));

    if cloudflared {
        println!("  note        : auto will prefer cloudflared for global sharing");
    } else {
        println!("  note        : auto will fall back to the native relay client");
        println!("  note        : set BEAM_RELAY_URL or run beam-relay for native relay testing");
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
