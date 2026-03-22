use anyhow::{Context, Result};
use beam::{relay::RelayState, relay_protocol::DEFAULT_RELAY_URL};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use url::Url;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("beam-relay: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let bind_addr: SocketAddr = std::env::var("BEAM_RELAY_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".to_string())
        .parse()
        .context("invalid BEAM_RELAY_BIND")?;
    let public_base_url = Url::parse(
        &std::env::var("BEAM_RELAY_PUBLIC_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string()),
    )
    .context("invalid BEAM_RELAY_PUBLIC_URL")?;

    let listener = TcpListener::bind(bind_addr)
        .await
        .context("failed to bind beam relay listener")?;

    println!("beam-relay listening on {bind_addr}");
    println!("public base URL: {public_base_url}");

    axum::serve(listener, RelayState::router(public_base_url))
        .await
        .context("beam relay server stopped unexpectedly")?;

    Ok(())
}
