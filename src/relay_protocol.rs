use anyhow::{Result, bail};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

pub const DEFAULT_RELAY_URL: &str = "http://127.0.0.1:8787";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelaySessionCreateRequest {
    pub download_name: String,
    pub expires_at_unix: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelaySessionCreateResponse {
    pub public_id: String,
    pub public_url: String,
    pub websocket_url: String,
    pub secret: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayToClientMessage {
    RequestStart {
        request_id: u64,
        method: String,
        path: String,
        query: Option<String>,
        headers: Vec<HeaderPair>,
    },
    SessionClose {
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientToRelayMessage {
    ResponseStart {
        request_id: u64,
        status: u16,
        headers: Vec<HeaderPair>,
    },
    ResponseEnd {
        request_id: u64,
    },
    ResponseError {
        request_id: u64,
        status: u16,
        message: String,
    },
    SessionClose {
        reason: String,
    },
}

pub fn headers_to_pairs(headers: &HeaderMap, allow: fn(&HeaderName) -> bool) -> Vec<HeaderPair> {
    headers
        .iter()
        .filter(|(name, _)| allow(name))
        .filter_map(|(name, value)| {
            value.to_str().ok().map(|value| HeaderPair {
                name: name.as_str().to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

pub fn pairs_to_headers(pairs: &[HeaderPair]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for pair in pairs {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(pair.name.as_bytes()),
            HeaderValue::from_str(&pair.value),
        ) {
            headers.append(name, value);
        }
    }
    headers
}

pub fn encode_body_chunk(request_id: u64, bytes: Bytes) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + bytes.len());
    payload.extend_from_slice(&request_id.to_be_bytes());
    payload.extend_from_slice(bytes.as_ref());
    payload
}

pub fn decode_body_chunk(payload: &[u8]) -> Result<(u64, Bytes)> {
    if payload.len() < 8 {
        bail!("relay body frame is too short");
    }

    let request_id = u64::from_be_bytes(payload[..8].try_into().expect("8-byte request id"));
    Ok((request_id, Bytes::copy_from_slice(&payload[8..])))
}
