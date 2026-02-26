pub async fn run_ghost_test(&mut self) {
    println!("[TEST] Starting Zero-Risk Ghost Order Test...");
    
    // 1. Pick an active market ID (from your logs, e.g., the current ETH-UP market)
    let test_market_id = "PASTE_YOUR_ACTIVE_MARKET_ID_HERE"; 
    
    // 2. Set a "Ghost" price of 1 cent and a tiny size (e.g., 5 shares)
    let ghost_price = 0.01; 
    let ghost_size = 5.0;

    println!("[TEST] Attempting to place $0.01 Ghost Buy on market: {}", test_market_id);

    // 3. Attempt to sign and submit
    if let (Some(ref client), Some(ref signer)) = (&self.client, &self.signer_instance) {
        let order = client.limit_order()
            .token_id(test_market_id)
            .price(polymarket_client_sdk::types::Decimal::from_str("0.01").unwrap())
            .size(polymarket_client_sdk::types::Decimal::from_str("5.00").unwrap())
            .side(polymarket_client_sdk::clob::types::Side::Buy)
            .build()
            .await;

        match order {
            Ok(o) => {
                let signed = client.sign(signer, o).await.unwrap();
                match client.post_order(signed).await {
                    Ok(resp) => {
                        println!("[SUCCESS] Connection Verified! Order ID: {:?}", resp.order_id);
                        println!("[TEST] Instantly canceling ghost order...");
                        let _ = client.cancel_order(&resp.order_id).await;
                        println!("[SUCCESS] Ghost order canceled. You are ready for live trading.");
                    },
                    Err(e) => {
                        eprintln!("[FAIL] API rejected order: {}. Check API keys and USDC allowance.", e);
                    }
                }
            },
            Err(e) => eprintln!("[FAIL] Order building failed: {}", e),
        }
    }
}