// Production snippet: extracted from `eva01/src/jito_bundle.rs`
// Purpose: show our real Jito Block Engine bundle submission implementation (JSON-RPC over HTTP),
// including throttling, retry/backoff, endpoint fallback, and base64->base58 encoding fallback.
//
// Note: this file is provided for review only; it is not compiled as part of this demo crate.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use bs58;
use lazy_static::lazy_static;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use std::sync::Mutex;
use std::time::{Duration, Instant};

lazy_static! {
    static ref JITO_LAST_REQ_AT: Mutex<Instant> =
        Mutex::new(Instant::now() - Duration::from_secs(10));
    static ref CACHED_TIP_ACCOUNTS: Mutex<Option<Vec<Pubkey>>> = Mutex::new(None);
}

fn jito_min_interval_ms_for_method(method: &str) -> u64 {
    // Critical path: bundle submission. Default to 0ms (no artificial sleep).
    // Tip endpoints can be aggressively rate limited, so keep a small default throttle there.
    match method {
        "sendBundle" | "getBundleStatuses" => std::env::var("JITO_SEND_BUNDLE_MIN_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        "getTipAccounts" => std::env::var("JITO_TIP_ACCOUNTS_MIN_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1200),
        // tip_floor is REST, but we still use the same global throttle knob for safety.
        _ => std::env::var("JITO_OTHER_MIN_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(250),
    }
}

#[derive(Clone)]
pub struct JitoBundleClient {
    http: Client,
    urls: Vec<String>,
}

impl JitoBundleClient {
    pub fn new(urls: Vec<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build reqwest client");
        let urls: Vec<String> = urls
            .into_iter()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Self { http, urls }
    }

    pub fn get_tip_accounts(&self) -> Result<Vec<Pubkey>> {
        if let Some(cached) = CACHED_TIP_ACCOUNTS.lock().unwrap().clone() {
            return Ok(cached);
        }

        // Jito Block Engine JSON-RPC method
        let req = JsonRpcRequest::<Vec<serde_json::Value>> {
            jsonrpc: "2.0",
            id: 1,
            method: "getTipAccounts",
            params: vec![],
        };

        let body = self.post_jsonrpc_with_fallback(&req, "getTipAccounts")?;
        let resp: JsonRpcResponse<Vec<String>> = serde_json::from_str(&body)
            .map_err(|e| anyhow!("Jito getTipAccounts JSON parse error: {e} (body={body})"))?;
        let result = resp.into_result()?;

        let mut out = Vec::with_capacity(result.len());
        for s in result {
            out.push(
                s.parse::<Pubkey>()
                    .map_err(|e| anyhow!("Invalid tip account pubkey {s}: {e}"))?,
            );
        }

        *CACHED_TIP_ACCOUNTS.lock().unwrap() = Some(out.clone());
        Ok(out)
    }

    pub fn send_bundle(&self, txs_bincode: Vec<Vec<u8>>) -> Result<String> {
        let encoded_base64: Vec<String> = txs_bincode
            .iter()
            .map(|bytes| BASE64_STANDARD.encode(bytes))
            .collect();

        // Jito Block Engine JSON-RPC method
        let req_base64 = JsonRpcRequest::<Vec<serde_json::Value>> {
            jsonrpc: "2.0",
            id: 1,
            method: "sendBundle",
            params: vec![serde_json::Value::Array(
                encoded_base64
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            )],
        };

        // Most block engine deployments expect base58-encoded tx bytes, but some accept base64.
        // Try base64 first; if the BE rejects with "transaction #0 could not be decoded", retry with base58.
        match self.post_jsonrpc_with_fallback(&req_base64, "sendBundle") {
            Ok(body) => {
                let resp: JsonRpcResponse<String> = serde_json::from_str(&body)
                    .map_err(|e| anyhow!("Jito sendBundle JSON parse error: {e} (body={body})"))?;
                resp.into_result()
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("could not be decoded") || msg.contains("transaction #0") {
                    let encoded_base58: Vec<String> = txs_bincode
                        .iter()
                        .map(|bytes| bs58::encode(bytes).into_string())
                        .collect();

                    let req_base58 = JsonRpcRequest::<Vec<serde_json::Value>> {
                        jsonrpc: "2.0",
                        id: 1,
                        method: "sendBundle",
                        params: vec![serde_json::Value::Array(
                            encoded_base58
                                .into_iter()
                                .map(serde_json::Value::String)
                                .collect(),
                        )],
                    };

                    let body = self.post_jsonrpc_with_fallback(&req_base58, "sendBundle")?;
                    let resp: JsonRpcResponse<String> =
                        serde_json::from_str(&body).map_err(|e| {
                            anyhow!("Jito sendBundle JSON parse error: {e} (body={body})")
                        })?;
                    return resp.into_result();
                }

                Err(anyhow!(msg))
            }
        }
    }

    /// Best-effort bundle status fetch. Useful for mapping a returned bundle id -> landed tx signatures.
    pub fn get_bundle_statuses(&self, bundle_ids: Vec<String>) -> Result<Vec<BundleStatus>> {
        let req = JsonRpcRequest::<Vec<serde_json::Value>> {
            jsonrpc: "2.0",
            id: 1,
            method: "getBundleStatuses",
            params: vec![serde_json::Value::Array(
                bundle_ids
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            )],
        };

        let body = self.post_jsonrpc_with_fallback(&req, "getBundleStatuses")?;
        let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            anyhow!("getBundleStatuses JSON parse error: {e} (body={body})")
        })?;

        if let Ok(resp) = serde_json::from_value::<JsonRpcResponse<BundleStatusesResult>>(v.clone())
        {
            let result = resp.into_result()?;
            return Ok(result.value.unwrap_or_default());
        }

        if let Ok(resp) = serde_json::from_value::<JsonRpcResponse<Vec<BundleStatus>>>(v.clone()) {
            return resp.into_result();
        }

        Err(anyhow!("Unrecognized getBundleStatuses response: {}", v))
    }

    pub fn wait_for_landed_signatures(
        &self,
        bundle_id: &str,
        timeout: Duration,
    ) -> Result<Vec<String>> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let statuses = self.get_bundle_statuses(vec![bundle_id.to_string()])?;
            if let Some(st) = statuses.first() {
                if let Some(txs) = st.transactions.as_ref() {
                    if !txs.is_empty() {
                        return Ok(txs.clone());
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(vec![])
    }

    pub fn get_tip_floor_lamports(
        &self,
        tip_floor_url: &str,
        percentile: u8,
        use_ema: bool,
        min_lamports: u64,
        max_lamports: u64,
    ) -> Result<u64> {
        self.throttle(jito_min_interval_ms_for_method("tipFloor"));
        let floors: Vec<TipFloor> = self
            .http
            .get(tip_floor_url)
            .send()?
            .error_for_status()?
            .json()?;

        let first = floors
            .first()
            .ok_or_else(|| anyhow!("tip_floor returned empty response"))?;

        // Values are in SOL (as floats). Convert to lamports conservatively.
        let sol = if use_ema {
            if percentile == 50 {
                first
                    .ema_landed_tips_50th_percentile
                    .unwrap_or(first.landed_tips_50th_percentile)
            } else {
                first.get_landed_percentile(percentile)?
            }
        } else {
            first.get_landed_percentile(percentile)?
        };

        let mut lamports = (sol * LAMPORTS_PER_SOL as f64).ceil() as u64;
        lamports = lamports.max(min_lamports);
        lamports = lamports.min(max_lamports);
        Ok(lamports)
    }

    fn throttle(&self, min_interval_ms: u64) {
        if min_interval_ms == 0 {
            return;
        }
        let min_interval = Duration::from_millis(min_interval_ms);
        let mut last = JITO_LAST_REQ_AT.lock().unwrap();
        let now = Instant::now();
        if let Some(next_ok) = last.checked_add(min_interval) {
            if next_ok > now {
                std::thread::sleep(next_ok - now);
            }
        }
        *last = Instant::now();
    }

    fn post_jsonrpc_with_fallback<T: Serialize>(&self, req: &T, method: &str) -> Result<String> {
        if self.urls.is_empty() {
            return Err(anyhow!("No Jito block engine URLs configured"));
        }

        let mut last_err: Option<anyhow::Error> = None;
        for url in self.urls.iter() {
            match self.post_jsonrpc_with_retry_to_url(url, req, method) {
                Ok(body) => return Ok(body),
                Err(e) => {
                    if e.to_string().contains("non-retryable") {
                        return Err(e);
                    }
                    last_err = Some(e);
                    continue;
                }
            }
        }

        Err(anyhow!(
            "All Jito endpoints failed (last error: {})",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ))
    }

    fn post_jsonrpc_with_retry_to_url<T: Serialize>(
        &self,
        url: &str,
        req: &T,
        method: &str,
    ) -> Result<String> {
        for attempt in 0..3 {
            self.throttle(jito_min_interval_ms_for_method(method));

            let resp = match self.http.post(url).json(req).send() {
                Ok(r) => r,
                Err(e) => {
                    if attempt < 2 {
                        std::thread::sleep(Duration::from_secs((1u64 << attempt).min(8)));
                        continue;
                    }
                    return Err(anyhow!("Jito request error for {}: {}", url, e));
                }
            };

            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());

            if (status.as_u16() == 429 || status.is_server_error()) && attempt < 2 {
                let sleep_s = retry_after.unwrap_or_else(|| 1u64 << attempt);
                std::thread::sleep(Duration::from_secs(sleep_s.min(8)));
                continue;
            }

            let body = resp.text().unwrap_or_default();
            if !status.is_success() {
                if status.is_client_error() && status.as_u16() != 429 {
                    return Err(anyhow!(
                        "Jito non-retryable HTTP error {} for {} (body={})",
                        status,
                        url,
                        body
                    ));
                }
                return Err(anyhow!("Jito HTTP error {} for {} (body={})", status, url, body));
            }
            return Ok(body);
        }

        Err(anyhow!(
            "Jito request rate-limited (429) or errored after retries for {}",
            url
        ))
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct BundleStatusesResult {
    #[allow(dead_code)]
    context: Option<serde_json::Value>,
    value: Option<Vec<BundleStatus>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BundleStatus {
    #[serde(rename = "bundle_id", alias = "bundleId")]
    pub bundle_id: Option<String>,
    pub transactions: Option<Vec<String>>,
    #[allow(dead_code)]
    pub slot: Option<u64>,
    #[allow(dead_code)]
    pub status: Option<String>,
}

#[derive(Deserialize)]
struct TipFloor {
    #[allow(dead_code)]
    time: Option<String>,
    landed_tips_25th_percentile: f64,
    landed_tips_50th_percentile: f64,
    landed_tips_75th_percentile: f64,
    landed_tips_95th_percentile: f64,
    landed_tips_99th_percentile: f64,
    ema_landed_tips_50th_percentile: Option<f64>,
}

impl TipFloor {
    fn get_landed_percentile(&self, p: u8) -> Result<f64> {
        match p {
            25 => Ok(self.landed_tips_25th_percentile),
            50 => Ok(self.landed_tips_50th_percentile),
            75 => Ok(self.landed_tips_75th_percentile),
            95 => Ok(self.landed_tips_95th_percentile),
            99 => Ok(self.landed_tips_99th_percentile),
            _ => Err(anyhow!(
                "Unsupported Jito tip percentile {} (use 25,50,75,95,99)",
                p
            )),
        }
    }
}

#[derive(Serialize)]
struct JsonRpcRequest<T> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: T,
}

#[derive(Deserialize)]
struct JsonRpcResponse<T> {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
    #[allow(dead_code)]
    data: Option<serde_json::Value>,
}

impl<T> JsonRpcResponse<T> {
    #[allow(dead_code)]
    fn into_result(self) -> Result<T> {
        if let Some(err) = self.error {
            return Err(anyhow!("JSON-RPC error: {}", err.message));
        }
        self.result.ok_or_else(|| anyhow!("Missing result"))
    }
}


