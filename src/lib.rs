//! Minimal, standalone Jito Block Engine bundles (JSON-RPC over HTTP) demo client.
//!
//! This is extracted from a production codebase and intentionally focuses on:
//! - JSON-RPC request shapes (`sendBundle`, `getTipAccounts`, `getBundleStatuses`)
//! - endpoint fallback (multiple BE URLs)
//! - throttling + retry/backoff for 429/timeouts/5xx
//! - base64-first encoding with base58 retry (some BEs expect base58)

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use bs58;
use lazy_static::lazy_static;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

lazy_static! {
    static ref JITO_LAST_REQ_AT: Mutex<Instant> =
        Mutex::new(Instant::now() - Duration::from_secs(10));
}

fn jito_min_interval_ms_for_method(method: &str) -> u64 {
    // Bundle submission is typically on the critical path; default to 0ms (no artificial sleep).
    // Tip endpoints can be aggressively rate-limited; keep a small default throttle there.
    match method {
        "sendBundle" | "getBundleStatuses" => std::env::var("JITO_SEND_BUNDLE_MIN_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0),
        "getTipAccounts" => std::env::var("JITO_TIP_ACCOUNTS_MIN_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1200),
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
    /// `urls` can be either:
    /// - a full bundles JSON-RPC URL (ends with `/api/v1/bundles`), or
    /// - a base host like `https://frankfurt.mainnet.block-engine.jito.wtf` (we append the path).
    pub fn new(mut urls: Vec<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build reqwest client");

        // Normalize: trim, strip trailing '/', append bundles path if needed.
        for u in urls.iter_mut() {
            *u = u.trim().trim_end_matches('/').to_string();
            if !u.ends_with("/api/v1/bundles") {
                *u = format!("{}/api/v1/bundles", u);
            }
        }

        let urls = urls.into_iter().filter(|s| !s.is_empty()).collect();
        Self { http, urls }
    }

    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    pub fn get_tip_accounts(&self) -> Result<Vec<String>> {
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
        resp.into_result()
    }

    /// Send a bundle given *raw transaction bytes* (bincode of `Transaction`/`VersionedTransaction`).
    ///
    /// The BE expects strings: many deployments accept base58; some accept base64.
    /// We try base64 first (common across Solana JSON-RPC), and retry base58 on decode errors.
    pub fn send_bundle_bincode_txs(&self, txs_bincode: Vec<Vec<u8>>) -> Result<String> {
        let encoded_base64: Vec<String> = txs_bincode
            .iter()
            .map(|bytes| BASE64_STANDARD.encode(bytes))
            .collect();

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
                    let resp: JsonRpcResponse<String> = serde_json::from_str(&body).map_err(|e| {
                        anyhow!("Jito sendBundle JSON parse error: {e} (body={body})")
                    })?;
                    return resp.into_result();
                }

                Err(anyhow!(msg))
            }
        }
    }

    /// Best-effort status fetch. Response schemas vary slightly across deployments,
    /// so this parses both a `{ value: [...] }` wrapper and a raw array.
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
        // Retry 429 / timeouts / server errors with exponential backoff.
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
    pub context: Option<serde_json::Value>,
    pub value: Option<Vec<BundleStatus>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct BundleStatus {
    #[serde(rename = "bundle_id", alias = "bundleId")]
    pub bundle_id: Option<String>,
    /// Transaction signatures that landed for this bundle (when available).
    pub transactions: Option<Vec<String>>,
    #[allow(dead_code)]
    pub slot: Option<u64>,
    #[allow(dead_code)]
    pub status: Option<String>,
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
    fn into_result(self) -> Result<T> {
        if let Some(err) = self.error {
            return Err(anyhow!("JSON-RPC error: {}", err.message));
        }
        self.result.ok_or_else(|| anyhow!("Missing result"))
    }
}


