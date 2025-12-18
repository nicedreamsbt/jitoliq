use anyhow::{anyhow, Result};
use base64::Engine;
use jitoliq::JitoBundleClient;
use std::time::Duration;

fn env_vec(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn main() -> Result<()> {
    // Minimal demo CLI:
    // - prints configured endpoints
    // - calls getTipAccounts
    // - optionally submits a "dummy bundle" if user provides tx bytes (base64) via env
    //
    // This is intentionally not a full liquidator; it’s a transport/rate-limit demo for BE eval.
    let urls = env_vec("JITO_BLOCK_ENGINE_URLS");
    if urls.is_empty() {
        return Err(anyhow!(
            "Set JITO_BLOCK_ENGINE_URLS (comma-separated). Example: https://frankfurt.mainnet.block-engine.jito.wtf"
        ));
    }

    let client = JitoBundleClient::new(urls);
    eprintln!("Jito bundles JSON-RPC endpoints:");
    for u in client.urls() {
        eprintln!("  - {}", u);
    }

    let tips = client.get_tip_accounts()?;
    eprintln!("getTipAccounts: {} accounts (showing up to 5)", tips.len());
    for t in tips.iter().take(5) {
        eprintln!("  - {}", t);
    }

    // Optional: submit a bundle if tx bytes are provided.
    // Expect env `BUNDLE_TXS_BASE64_JSON` as a JSON array of base64 strings, where each string
    // is the raw transaction bytes (bincode).
    //
    // Note: production systems usually build the txs from Solana SDK structures; for demo,
    // providing raw bytes is enough to show the sendBundle transport path.
    if let Ok(raw) = std::env::var("BUNDLE_TXS_BASE64_JSON") {
        if !raw.trim().is_empty() {
            let txs_b64: Vec<String> = serde_json::from_str(&raw)
                .map_err(|e| anyhow!("Invalid BUNDLE_TXS_BASE64_JSON: {e}"))?;
            let mut txs: Vec<Vec<u8>> = Vec::with_capacity(txs_b64.len());
            for s in txs_b64 {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(s)
                    .map_err(|e| anyhow!("Invalid base64 tx bytes: {e}"))?;
                txs.push(bytes);
            }

            let bundle_id = client.send_bundle_bincode_txs(txs)?;
            eprintln!("sendBundle OK: bundle_id={}", bundle_id);

            if let Ok(sigs) = client.wait_for_landed_signatures(&bundle_id, Duration::from_secs(2))
            {
                if !sigs.is_empty() {
                    eprintln!("bundle landed tx signatures: {:?}", sigs);
                } else {
                    eprintln!("bundle signatures unknown (no landed sigs observed in 2s)");
                }
            }
        }
    }

    Ok(())
}


