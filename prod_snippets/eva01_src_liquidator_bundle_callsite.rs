// Production snippet: extracted from `eva01/src/liquidator.rs`
// Purpose: show our real call-site behavior around bundle submission:
// - how we build the bundle payload (crank/liquidation/tip ordering)
// - optional RPC fallback send after a delay for reliability
//
// Note: this file is provided for review only; it is not compiled as part of this demo crate.

// ... imports elided ...

fn schedule_rpc_fallback_send(&self, liq_tx: &solana_sdk::transaction::VersionedTransaction) {
    let delay_ms = self.jito_bundle_liquidations_rpc_fallback_delay_ms;
    if delay_ms == 0 {
        return;
    }

    // Serialize so we can move it into the thread without relying on Clone.
    let bytes = match bincode::serialize(liq_tx) {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to serialize liquidation tx for RPC fallback: {}", e);
            return;
        }
    };
    let liquidator_account = self.liquidator_account.clone();

    std::thread::spawn(move || {
        std::thread::sleep(StdDuration::from_millis(delay_ms));
        let tx: solana_sdk::transaction::VersionedTransaction = match bincode::deserialize(&bytes)
        {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to deserialize liquidation tx for RPC fallback: {}", e);
                return;
            }
        };
        match liquidator_account.submit_versioned_tx_rpc_no_confirm(&tx) {
            Ok(sig) => {
                info!("RPC fallback submitted liquidation tx signature={}", sig);
            }
            Err(e) => {
                // Common when bundle already landed: "already processed"
                debug!("RPC fallback submit failed: {:?}", e);
            }
        }
    });
}

fn try_bundle_liquidation_and_tip(
    &self,
    acc: &PreparedLiquidatableAccount,
    tokens_in_shortage: &mut HashSet<Pubkey>,
) -> Result<()> {
    if self.jito_block_engine_urls.is_empty() {
        return Err(anyhow!(
            "Jito bundle liquidations enabled but no Jito block engine URLs configured"
        ));
    }
    let client = JitoBundleClient::new(self.jito_block_engine_urls.clone());

    // Build liquidation tx (without sending) and get the blockhash it compiled with.
    let (liq_tx, blockhash) = self
        .liquidator_account
        .build_liquidation_tx_for_bundle(acc, &HashSet::new(), tokens_in_shortage)
        .map_err(|e| anyhow!("Failed to build liquidation tx for bundle: {:?}", e))?;

    // Tip account selection
    let tip_account = if let Some(pk) = self.jito_tip_account {
        pk
    } else if let Some(pk) = self.liquidator_account.jito_tip_account() {
        pk
    } else {
        let tips = client.get_tip_accounts()?;
        *tips
            .first()
            .ok_or_else(|| anyhow!("Jito getTipAccounts returned empty list"))?
    };

    // Tip selection (profit-tiered) then build a tip tx using the same blockhash.
    // ... tip selection elided ...
    let tip_tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &self.liquidator_account.signer.pubkey(),
            &tip_account,
            tip_lamports,
        )],
        Some(&self.liquidator_account.signer.pubkey()),
        &[&self.liquidator_account.signer],
        blockhash,
    );

    // Bundle: liquidation -> tip
    let bytes: Vec<Vec<u8>> = vec![bincode::serialize(&liq_tx)?, bincode::serialize(&tip_tx)?];
    let bundle_id = client.send_bundle(bytes)?;
    info!(
        "Submitted Jito bundle (liquidation + tip={} lamports) id={}",
        tip_lamports, bundle_id
    );

    // Optionally, also send to RPC after a delay (reliability fallback).
    self.schedule_rpc_fallback_send(&liq_tx);
    Ok(())
}


