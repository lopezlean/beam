use anyhow::Result;
use std::{
    io::{IsTerminal, stdout},
    process::{Command, Stdio},
};

pub async fn run() -> Result<()> {
    let cloudflared = command_exists("cloudflared", &["--version"]);
    let clipboard = clipboard_command().is_some();
    let terminal = stdout().is_terminal();
    let ansi = std::env::var("TERM")
        .map(|term| term != "dumb")
        .unwrap_or(false)
        && terminal;

    println!("Beam doctor");
    println!("  local https : ok");
    println!("  cloudflared : {}", pass_fail(cloudflared));
    println!("  clipboard   : {}", pass_fail(clipboard));
    println!("  terminal    : {}", pass_fail(terminal));
    println!("  ansi/unicode: {}", pass_fail(ansi));

    if cloudflared {
        println!("  note        : --global is ready to use on this machine");
    } else {
        println!("  note        : install cloudflared to use --global");
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
    Command::new(command)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn pass_fail(value: bool) -> &'static str {
    if value { "ok" } else { "missing" }
}
