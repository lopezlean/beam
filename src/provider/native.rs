use crate::{
    relay::relay_response_header_allowed,
    relay_protocol::{
        ClientToRelayMessage, DEFAULT_RELAY_URL, RelaySessionCreateRequest,
        RelaySessionCreateResponse, RelayToClientMessage, encode_body_chunk, headers_to_pairs,
        pairs_to_headers,
    },
};
use anyhow::{Context, Result, anyhow};
use axum::http::StatusCode;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{TunnelHandle, TunnelRuntime};

pub(super) fn relay_configured() -> bool {
    std::env::var_os("BEAM_RELAY_URL").is_some()
}

pub(super) async fn available_for_auto() -> bool {
    relay_configured() || local_default_native_relay_reachable().await
}

pub(super) async fn start(local_url: &str) -> Result<TunnelHandle> {
    let relay_url = relay_base_url()?;
    let client = Client::builder()
        .use_rustls_tls()
        .build()
        .context("failed to create Beam native relay HTTP client")?;
    let create_url = relay_url.join("v1/sessions").context("invalid relay URL")?;
    let request = RelaySessionCreateRequest {
        download_name: "beam".to_string(),
        expires_at_unix: unix_now() + 3600,
    };
    let session = client
        .post(create_url)
        .json(&request)
        .send()
        .await
        .context("failed to reach the Beam native relay")?
        .error_for_status()
        .context("Beam native relay rejected the session")?
        .json::<RelaySessionCreateResponse>()
        .await
        .context("failed to decode the Beam native relay response")?;

    let (status_tx, status_rx) = watch::channel("Connecting to Beam native relay".to_string());
    let shutdown = CancellationToken::new();
    let (ws_stream, _) = connect_async(session.websocket_url.as_str())
        .await
        .context("failed to open the Beam native relay websocket")?;
    let (mut ws_writer, mut ws_reader) = ws_stream.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Message>();
    let request_client = Client::builder()
        .use_rustls_tls()
        .build()
        .context("failed to create Beam local proxy HTTP client")?;

    let writer_shutdown = shutdown.clone();
    let writer_status = status_tx.clone();
    let writer_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = writer_shutdown.cancelled() => {
                    let _ = ws_writer.send(Message::Close(None)).await;
                    break;
                }
                maybe_message = outbound_rx.recv() => {
                    let Some(message) = maybe_message else { break; };
                    if ws_writer.send(message).await.is_err() {
                        let _ = writer_status.send("Beam native relay writer stopped".to_string());
                        break;
                    }
                }
            }
        }
    });

    let reader_shutdown = shutdown.clone();
    let reader_status = status_tx.clone();
    let local_origin = local_url.to_string();
    let reader_task = tokio::spawn(async move {
        while let Some(message_result) = ws_reader.next().await {
            let message = match message_result {
                Ok(message) => message,
                Err(error) => {
                    let _ = reader_status.send(format!("Beam native relay disconnected: {error}"));
                    break;
                }
            };

            match message {
                Message::Text(text) => {
                    let decoded = match serde_json::from_str::<RelayToClientMessage>(&text) {
                        Ok(message) => message,
                        Err(error) => {
                            let _ = reader_status
                                .send(format!("Beam native relay protocol error: {error}"));
                            break;
                        }
                    };

                    match decoded {
                        RelayToClientMessage::RequestStart {
                            request_id,
                            method,
                            path,
                            query,
                            headers,
                        } => {
                            let outbound_tx = outbound_tx.clone();
                            let request_client = request_client.clone();
                            let local_origin = local_origin.clone();
                            let reader_status = reader_status.clone();
                            tokio::spawn(async move {
                                if let Err(error) = proxy_relay_request(
                                    request_client,
                                    outbound_tx,
                                    &local_origin,
                                    request_id,
                                    &method,
                                    &path,
                                    query.as_deref(),
                                    &headers,
                                )
                                .await
                                {
                                    let _ = reader_status.send(format!(
                                        "Beam native relay request {request_id} failed: {error}"
                                    ));
                                }
                            });
                        }
                        RelayToClientMessage::SessionClose { reason } => {
                            let _ =
                                reader_status.send(format!("Beam native relay closed: {reason}"));
                            break;
                        }
                    }
                }
                Message::Close(_) => {
                    let _ = reader_status.send("Beam native relay closed".to_string());
                    break;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Binary(_) | Message::Frame(_) => {}
            }
        }

        reader_shutdown.cancel();
    });

    let _ = status_tx.send(format!("Beam native relay ready at {}", session.public_url));

    Ok(TunnelHandle {
        public_url: session.public_url,
        provider_name: "native",
        runtime: TunnelRuntime::Managed { shutdown },
        status_rx,
        tasks: vec![writer_task, reader_task],
    })
}

async fn proxy_relay_request(
    client: Client,
    outbound_tx: mpsc::UnboundedSender<Message>,
    local_origin: &str,
    request_id: u64,
    method: &str,
    path: &str,
    query: Option<&str>,
    headers: &[crate::relay_protocol::HeaderPair],
) -> Result<()> {
    let request_url = if let Some(query) = query {
        format!("{local_origin}{path}?{query}")
    } else {
        format!("{local_origin}{path}")
    };
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("unsupported proxied method {method}"))?;
    let mut request = client.request(method, &request_url);

    for (name, value) in pairs_to_headers(headers).iter() {
        request = request.header(name, value);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            send_control_message(
                &outbound_tx,
                ClientToRelayMessage::ResponseError {
                    request_id,
                    status: StatusCode::BAD_GATEWAY.as_u16(),
                    message: format!("Beam could not reach its local server: {error}"),
                },
            )?;
            return Err(error.into());
        }
    };

    send_control_message(
        &outbound_tx,
        ClientToRelayMessage::ResponseStart {
            request_id,
            status: response.status().as_u16(),
            headers: headers_to_pairs(response.headers(), relay_response_header_allowed),
        },
    )?;

    let mut body_stream = response.bytes_stream();
    while let Some(chunk_result) = body_stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                outbound_tx
                    .send(Message::Binary(encode_body_chunk(request_id, chunk).into()))
                    .map_err(|_| anyhow!("relay websocket sender stopped"))?;
            }
            Err(error) => {
                send_control_message(
                    &outbound_tx,
                    ClientToRelayMessage::ResponseError {
                        request_id,
                        status: StatusCode::BAD_GATEWAY.as_u16(),
                        message: format!("Beam relay response stream failed: {error}"),
                    },
                )?;
                return Err(error.into());
            }
        }
    }

    send_control_message(
        &outbound_tx,
        ClientToRelayMessage::ResponseEnd { request_id },
    )?;
    Ok(())
}

fn send_control_message(
    outbound_tx: &mpsc::UnboundedSender<Message>,
    message: ClientToRelayMessage,
) -> Result<()> {
    outbound_tx
        .send(Message::Text(
            serde_json::to_string(&message)
                .context("failed to serialize native relay control message")?
                .into(),
        ))
        .map_err(|_| anyhow!("relay websocket sender stopped"))
}

fn relay_base_url() -> Result<Url> {
    Url::parse(&std::env::var("BEAM_RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string()))
        .context("invalid BEAM_RELAY_URL")
}

async fn local_default_native_relay_reachable() -> bool {
    let relay_url = match Url::parse(DEFAULT_RELAY_URL) {
        Ok(url) => url,
        Err(_) => return false,
    };

    let Some(host) = relay_url.host_str() else {
        return false;
    };

    if host != "127.0.0.1" && host != "localhost" {
        return false;
    }

    let health_url = match relay_url.join("healthz") {
        Ok(url) => url,
        Err(_) => return false,
    };

    let client = match Client::builder()
        .use_rustls_tls()
        .timeout(Duration::from_millis(400))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    matches!(
        client.get(health_url).send().await,
        Ok(response) if response.status().is_success()
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
