use super::*;
use thiserror::Error;

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Error)]
pub(super) enum RuntimeRpcError {
    #[error("failed to build rpc http client: {source}")]
    BuildClient { source: reqwest::Error },
    #[error("rpc request failed for method `{method}`: {source}")]
    Request {
        method: &'static str,
        source: reqwest::Error,
    },
    #[error("rpc method `{method}` failed with status {status}: {source}")]
    HttpStatus {
        method: &'static str,
        status: reqwest::StatusCode,
        source: reqwest::Error,
    },
    #[error("rpc method `{method}` returned invalid json: {source}")]
    InvalidJson {
        method: &'static str,
        source: reqwest::Error,
    },
    #[error("rpc method `{method}` error {code}: {message}")]
    RpcMethod {
        method: &'static str,
        code: i64,
        message: String,
    },
    #[error("rpc method `{method}` returned neither result nor error")]
    MissingResultOrError { method: &'static str },
    #[error("slot leader chunk overflow for value {value}: {source}")]
    SlotLeaderChunkOverflow {
        value: usize,
        source: std::num::TryFromIntError,
    },
    #[error("slot leader offset overflow for value {value}: {source}")]
    SlotLeaderOffsetOverflow {
        value: usize,
        source: std::num::TryFromIntError,
    },
    #[error("rpc returned invalid slot leader pubkey for slot {slot}: {value}: {source}")]
    InvalidSlotLeaderPubkey {
        slot: u64,
        value: String,
        source: solana_pubkey::ParsePubkeyError,
    },
    #[error("slot leader fetched count conversion failed for value {value}: {source}")]
    SlotLeaderFetchedCountOverflow {
        value: usize,
        source: std::num::TryFromIntError,
    },
    #[error("rpc returned no slot leaders for verification bootstrap")]
    NoSlotLeaders,
}

fn build_rpc_http_client() -> Result<reqwest::Client, RuntimeRpcError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|source| RuntimeRpcError::BuildClient { source })
}

pub(super) async fn load_slot_leaders_from_rpc(
    rpc_url: &str,
    history_slots: u64,
    fetch_slots: usize,
) -> Result<Vec<(u64, [u8; 32])>, RuntimeRpcError> {
    const MAX_SLOT_LEADERS_PER_REQUEST: usize = 5_000;

    let client = build_rpc_http_client()?;
    let current_slot: u64 = rpc_call(&client, rpc_url, "getSlot", serde_json::json!([])).await?;
    let mut start_slot = current_slot.saturating_sub(history_slots);
    let mut remaining = fetch_slots.max(1);
    let mut slot_leaders = Vec::with_capacity(fetch_slots.max(1));

    while remaining > 0 {
        let chunk = remaining.min(MAX_SLOT_LEADERS_PER_REQUEST);
        let chunk_u64 =
            u64::try_from(chunk).map_err(|source| RuntimeRpcError::SlotLeaderChunkOverflow {
                value: chunk,
                source,
            })?;
        let leaders: Vec<String> = rpc_call(
            &client,
            rpc_url,
            "getSlotLeaders",
            serde_json::json!([start_slot, chunk_u64]),
        )
        .await?;
        if leaders.is_empty() {
            break;
        }
        let mut fetched = 0_usize;
        for (offset, pubkey) in leaders.into_iter().enumerate() {
            let offset = u64::try_from(offset).map_err(|source| {
                RuntimeRpcError::SlotLeaderOffsetOverflow {
                    value: offset,
                    source,
                }
            })?;
            let slot = start_slot.saturating_add(offset);
            let pubkey = Pubkey::from_str(&pubkey).map_err(|source| {
                RuntimeRpcError::InvalidSlotLeaderPubkey {
                    slot,
                    value: pubkey.clone(),
                    source,
                }
            })?;
            slot_leaders.push((slot, pubkey.to_bytes()));
            fetched = fetched.saturating_add(1);
        }
        if fetched < chunk {
            break;
        }
        start_slot = start_slot.saturating_add(u64::try_from(fetched).map_err(|source| {
            RuntimeRpcError::SlotLeaderFetchedCountOverflow {
                value: fetched,
                source,
            }
        })?);
        remaining = remaining.saturating_sub(fetched);
    }

    if slot_leaders.is_empty() {
        return Err(RuntimeRpcError::NoSlotLeaders);
    }
    Ok(slot_leaders)
}

pub(super) async fn load_current_slot_from_rpc(rpc_url: &str) -> Result<u64, RuntimeRpcError> {
    let client = build_rpc_http_client()?;
    rpc_call(&client, rpc_url, "getSlot", serde_json::json!([])).await
}

async fn rpc_call<T: DeserializeOwned>(
    client: &reqwest::Client,
    rpc_url: &str,
    method: &'static str,
    params: serde_json::Value,
) -> Result<T, RuntimeRpcError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let response = client
        .post(rpc_url)
        .json(&payload)
        .send()
        .await
        .map_err(|source| RuntimeRpcError::Request { method, source })?;
    let status = response.status();
    let response = response
        .error_for_status()
        .map_err(|source| RuntimeRpcError::HttpStatus {
            method,
            status,
            source,
        })?;
    let parsed: JsonRpcResponse<T> = response
        .json()
        .await
        .map_err(|source| RuntimeRpcError::InvalidJson { method, source })?;
    if let Some(result) = parsed.result {
        return Ok(result);
    }
    if let Some(error) = parsed.error {
        return Err(RuntimeRpcError::RpcMethod {
            method,
            code: error.code,
            message: error.message,
        });
    }
    Err(RuntimeRpcError::MissingResultOrError { method })
}
