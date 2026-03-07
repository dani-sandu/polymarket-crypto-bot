use core_shared::{Market, TokenDirection};
use ethers::prelude::*;
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::Decimal;
use polymarket_client_sdk::auth::state::Authenticated; 
use polymarket_client_sdk::auth::{Signer as SDKSigner, Normal};
use polymarket_client_sdk::POLYGON;
use alloy_signer_local::LocalSigner;
use k256::ecdsa::SigningKey;
use std::str::FromStr;
use std::sync::Arc;
use std::collections::HashMap;
use std::env;
use chrono::Utc;
use tokio::sync::watch;

use crate::state::{PersistentState, BotState};
use crate::atr::AtrMonitor;
use crate::velocity::VelocityLockout;
use crate::config::MarketConfig;

pub struct StrategyEngine {
    pub trading_enabled: bool,
    pub asset: String,
    pub signature_type: u8,
    pub funder: Option<Address>,
    pub time_offset: i64,
    
    // Auth / SDK
    pub client: Option<ClobClient<Authenticated<Normal>>>,
    pub signer_instance: Option<LocalSigner<SigningKey>>,
    
    // Shared HTTP clients (reused across calls)
    pub http_client: reqwest::Client,
    pub rpc_provider: Option<Provider<Http>>,
    
    // Risk & Loop Counters
    pub atr: AtrMonitor,
    pub velocity: VelocityLockout,
    pub state: PersistentState,
    pub last_oracle_log: i64,
    pub last_recon_ms: i64,
    pub traded_markets: std::collections::HashSet<String>,
    
    // Watch Receivers
    pub binance_rx: watch::Receiver<f64>,
    pub coinbase_rx: watch::Receiver<f64>,
    pub dvol_rx: watch::Receiver<f64>,
    pub polymarket_rx: watch::Receiver<HashMap<String, Market>>,
    pub market_id_rx: watch::Receiver<Option<String>>,
}

impl StrategyEngine {
    pub async fn new(
        trading_enabled: bool,
        asset: String,
        signature_type: u8,
        funder: Option<Address>,
        time_offset: i64,
        binance_rx: watch::Receiver<f64>,
        coinbase_rx: watch::Receiver<f64>,
        dvol_rx: watch::Receiver<f64>,
        polymarket_rx: watch::Receiver<HashMap<String, Market>>,
        market_id_rx: watch::Receiver<Option<String>>,
    ) -> Self {
        let state = PersistentState::load();
        let atr = AtrMonitor::new().await;
        let velocity = VelocityLockout::new(30.0, 2);

        let http_client = reqwest::Client::new();
        let rpc_provider = env::var("POLYGON_RPC_URL")
            .ok()
            .and_then(|url| Provider::<Http>::try_from(url).ok());

        Self {
            trading_enabled,
            asset,
            signature_type,
            funder,
            time_offset,
            client: None,
            signer_instance: None,
            http_client,
            rpc_provider,
            atr,
            velocity,
            state,
            last_oracle_log: 0,
            last_recon_ms: 0,
            traded_markets: std::collections::HashSet::new(),
            binance_rx,
            coinbase_rx,
            dvol_rx,
            polymarket_rx,
            market_id_rx,
        }
    }

    pub async fn send_telegram_alert(message: &str) {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").unwrap_or_default();

        // If you forgot to add the keys, just skip sending
        if token.is_empty() || chat_id.is_empty() { return; }

        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        
        // Fire and forget (won't slow down the bot)
        let _ = reqwest::Client::new().post(&url)
            .form(&[("chat_id", &chat_id), ("text", &message.to_string())])
            .send()
            .await;
    }

    pub async fn execute_tick(&mut self, binance_price: f64, coinbase_price: f64, config: &MarketConfig) {
        let now_ms = Utc::now().timestamp_millis() + (self.time_offset * 1000);
        let now_sec = now_ms / 1000;
        let dvol = *self.dvol_rx.borrow();
        let active_dvol = if dvol > 0.0 { dvol } else { self.atr.current_atr() * 0.5 };
        let markets = self.polymarket_rx.borrow().clone();

        // --- DELTA-NEUTRAL KILLSWITCH ---
        // Ensure Binance and Coinbase agree on the "True Price" of the asset.
        // If they diverge by more than the threshold, the market is structurally broken.
        let exchange_divergence = (binance_price - coinbase_price).abs();
        
        if exchange_divergence > config.killswitch_threshold && coinbase_price > 0.0 {
            if now_sec % 5 == 0 {
                println!("[KILLSWITCH] Market De-Peg! Binance: ${:.2} | Coinbase: ${:.2} | Divergence: ${:.2}", 
                    binance_price, coinbase_price, exchange_divergence);
            }
            return; // Abort all trading evaluations until exchanges realign
        }

        // 1. STATE MACHINE LOCK CHECK & TIMEOUT
        if matches!(self.state.state, BotState::PendingBuy | BotState::PendingSell) {
            // Let the chain resolve it every 2 seconds, OR immediately force resolution if > 5s timeout hit
            if now_ms - self.last_recon_ms > 2000 || now_ms - self.state.pending_since > 5000 { 
                self.last_recon_ms = now_ms;
                self.reconcile_with_chain().await; 
            }
            return; // Stay locked until recon resolves it
        }

        // 2. EMERGENCY EXITS (Wobble Defense)
        if self.state.state == BotState::InPosition {
            // Periodic sanity check of the blockchain (every 15s)
            if now_ms - self.last_recon_ms > 15000 {
                self.last_recon_ms = now_ms;
                self.reconcile_with_chain().await;
            }
            
            if let Some(market_id) = &self.state.active_market_id {
                if let Some(market) = markets.get(market_id) {
                    let time_to_expiry = market.expiration - (now_ms / 1000);
                    let gap = (binance_price - market.strike_price).abs();
                    let coinbase_crash = coinbase_price > 0.0 && binance_price > 0.0 && (coinbase_price - binance_price).abs() > (binance_price * 0.0015);
                    let gap_collapse = gap < (binance_price * 0.0006) && time_to_expiry <= 10; 

                    if coinbase_crash || gap_collapse {
                        let reason = if coinbase_crash { "COINBASE WHALE DUMP" } else { "GAP COLLAPSE" };
                        println!("[EXIT] EMERGENCY EJECT: {}", reason);
                        self.emergency_sell(market).await;
                        return;
                    }

                    if time_to_expiry <= 0 {
                        // 1. Determine Settlement ($1.00 for win, $0.00 for loss)
                        let settlement = if market.direction == TokenDirection::Up {
                            if binance_price > market.strike_price { 1.0 } else { 0.0 }
                        } else {
                            if binance_price <= market.strike_price { 1.0 } else { 0.0 }
                        };
                        
                        // 2. Calculate PnL
                        let pnl = (settlement - self.state.entry_price) * self.state.position_size;
                        println!("[EXPIRY] Settled at ${:.2} | PnL: ${:.4}", settlement, pnl);
                        
                        if settlement > 0.5 {
                            let msg = format!("✅ WINNER!\nSuccessfully sold position.\nBalance is now: ${:.2}", self.state.simulated_balance + pnl);
                            Self::send_telegram_alert(&msg).await;
                        } else {
                            let msg = "❌ LOSS.\nPosition expired OTM. Hunting for next setup...";
                            Self::send_telegram_alert(msg).await;
                        }
                        
                        // 3. Update Balance (Simulation only. Prod relies on chain sync later)
                        if !self.trading_enabled {
                            self.state.simulated_balance += pnl;
                        }
                        
                        // 4. Force State Machine Reset
                        self.state.reset();
                        return;
                    }
                }
            }
        }

        self.atr.update_from_tick(binance_price, now_ms / 1000);
        self.velocity.update(binance_price);

        // 3. ENTRY MATRIX — Probability-Edge Strategy
        //
        // Overview:
        //   1. Compute the theoretical probability that BTC finishes above the strike
        //      using a normal-CDF model with ATR-adjusted volatility.
        //   2. Derive the fair value for the UP and DOWN tokens.
        //   3. Compare fair value to the Polymarket price — the difference is our "edge".
        //   4. Use the gap tiers from MarketConfig to set the minimum edge required:
        //      - Tier 1 (gap >= 180): high conviction, accept edge >= 4%
        //      - Tier 2 (gap >= 120): medium conviction, require edge >= 7%
        //      - Tier 3 (gap >= 80):  low conviction, require edge >= 12%
        //      - Below Tier 3: skip — not enough directional signal.
        //   5. Only buy the token whose direction is favored (UP if price > strike, DOWN otherwise).
        //
        if self.state.state == BotState::Idle {
            let current_atr = self.atr.current_atr();

            for market in markets.values() {
                if self.traded_markets.contains(&market.id) { continue; }
                let time_to_expiry = market.expiration - (now_ms / 1000);
                if time_to_expiry <= 4 || time_to_expiry > 20 { continue; }
                if self.velocity.is_locked() { continue; }

                let mkt_price = market.last_price;
                if mkt_price < 0.01 || mkt_price > 0.99 { continue; }

                let gap = (binance_price - market.strike_price).abs();

                // Compute spread from the orderbook
                let bid = market.orderbook.best_bid().unwrap_or(0.0);
                let ask = market.orderbook.best_ask().unwrap_or(1.0);
                let spread = (ask - bid).abs();
                if spread > config.max_spread { continue; }

                // --- Determine minimum edge required based on gap tier ---
                let min_edge = if gap >= config.tier_1_gap {
                    0.04 // Tier 1: strong directional move, accept 4% edge
                } else if gap >= config.tier_2_gap {
                    0.07 // Tier 2: moderate move, need 7% edge
                } else if gap >= config.tier_3_gap {
                    0.12 // Tier 3: small move, need 12% edge
                } else {
                    continue; // Gap too small — no directional conviction
                };

                // --- Calculate theoretical fair value ---
                // P(UP wins) = probability BTC finishes above strike
                let p_up = Self::calculate_probability(
                    binance_price,
                    market.strike_price,
                    time_to_expiry,
                    current_atr,
                );

                // Fair value for this specific token direction
                let fair_value = match market.direction {
                    TokenDirection::Up   => p_up,
                    TokenDirection::Down => 1.0 - p_up,
                };

                // --- Edge = how much cheaper the market is vs fair value ---
                let edge = fair_value - mkt_price;

                // --- Cross-exchange confirmation ---
                // Require Coinbase to agree on direction (if available)
                let coinbase_confirms = if coinbase_price > 0.0 {
                    match market.direction {
                        TokenDirection::Up   => coinbase_price > market.strike_price,
                        TokenDirection::Down => coinbase_price <= market.strike_price,
                    }
                } else {
                    true // No Coinbase data — don't block
                };

                if edge >= min_edge && fair_value > 0.60 && coinbase_confirms {
                    // Buy at the best ask (or mkt_price as a limit)
                    let buy_price = ask.min(mkt_price + 0.01); // Aggressive but capped
                    println!(
                        "[STRATEGY] {} | Gap ${:.0} (Tier {}) | Fair {:.2} vs Mkt {:.2} | Edge {:.1}% | TTE {}s",
                        if market.direction == TokenDirection::Up { "UP" } else { "DOWN" },
                        gap,
                        if gap >= config.tier_1_gap { 1 } else if gap >= config.tier_2_gap { 2 } else { 3 },
                        fair_value,
                        mkt_price,
                        edge * 100.0,
                        time_to_expiry
                    );
                    self.execute_buy(market, buy_price).await;
                    break;
                }
            }
        }

        if now_sec != self.last_oracle_log {
            self.last_oracle_log = now_sec;
            if now_sec % 5 == 0 {
                let mut spread = 0.0;
                let mut mkt_price = 0.0;
                if let Some(m) = markets.values().next() {
                    let bid = m.orderbook.best_bid().unwrap_or(0.0);
                    let ask = m.orderbook.best_ask().unwrap_or(1.0);
                    spread = (ask - bid).abs();
                    mkt_price = m.last_price;
                }
                println!("[HFT] {:?} | Bal ${:.2} | {} ${:.2} | DVOL {:.1} | Mkt ${:.4} | Spread {:.4}", 
                    self.state.state, self.state.simulated_balance, config.ticker, binance_price, active_dvol, mkt_price, spread);
            }
        }
        
        // Sync active market ID from watch channel
        if let Some(new_id) = self.market_id_rx.borrow().clone() {
            if self.state.active_market_id.is_none() {
                self.state.active_market_id = Some(new_id);
            }
        }
    }

    pub fn calculate_probability(current_price: f64, strike_price: f64, time_remaining_sec: i64, atr: f64) -> f64 {
        if time_remaining_sec <= 0 {
            return if current_price > strike_price { 1.0 } else { 0.0 };
        }
        let distance = current_price - strike_price;
        // Use ATR-derived per-second volatility instead of a hardcoded constant.
        // ATR is over 5-min candles (300s). Scale to per-second: ATR / sqrt(300).
        // Fallback to 9.0 if ATR is unreasonable.
        let volatility_per_sec = if atr > 1.0 { atr / (300.0_f64).sqrt() } else { 9.0 };
        let sigma = volatility_per_sec * (time_remaining_sec as f64).sqrt();
        if sigma < 1e-9 { return if distance > 0.0 { 1.0 } else { 0.0 }; }
        let z_score = distance / sigma;
        0.5 * (1.0 + libm::erf(z_score / std::f64::consts::SQRT_2))
    }

    async fn execute_buy(&mut self, market: &Market, limit_price: f64) {
        let size = (self.state.simulated_balance / limit_price * 100.0).floor() / 100.0;
        if size < 5.2 { return; } 

        if self.trading_enabled {
            self.state.state = BotState::PendingBuy;
            self.state.active_market_id = Some(market.id.clone()); 
            self.state.pending_since = Utc::now().timestamp_millis() + (self.time_offset * 1000);
            self.state.entry_price = limit_price;
            self.state.position_size = size;
            self.state.save();
            
            if self.sign_and_submit(market, limit_price, size, Side::Buy).await {
                // FIX: Only ban the market from re-buys if the API actually accepted the order!
                self.traded_markets.insert(market.id.clone());
                println!("[PROD] Buy Order Submitted: {}@${:.4}", size, limit_price);
                let msg = format!("🎯 TRADE PLACED!\nAsset: {}\nPrice: ${:.4}\nWaiting for 5m settlement...", self.asset, limit_price);
                Self::send_telegram_alert(&msg).await;
            } else {
                self.state.reset();
            }
        } else {
            self.state.state = BotState::InPosition;
            self.state.position_size = size;
            self.state.entry_price = limit_price;
            self.state.active_market_id = Some(market.id.clone());
            self.traded_markets.insert(market.id.clone());
            self.state.save();
            println!("[SIM] Entry at ${:.4}", limit_price);
            let msg = format!("🎯 TRADE PLACED!\nAsset: {}\nPrice: ${:.4}\nWaiting for 5m settlement...", self.asset, limit_price);
            Self::send_telegram_alert(&msg).await;
        }
    }

    async fn emergency_sell(&mut self, market: &Market) {
        let size = (self.state.position_size * 100.0).floor() / 100.0;
        if size < 5.0 { return; }

        let sell_price = (market.orderbook.best_bid().unwrap_or(market.last_price) - 0.15).max(0.01);

        if self.trading_enabled {
            self.state.state = BotState::PendingSell;
            self.state.pending_since = Utc::now().timestamp_millis() + (self.time_offset * 1000);
            self.state.save();
            self.sign_and_submit(market, sell_price, size, Side::Sell).await;
        } else {
            let pnl = (sell_price - self.state.entry_price) * size;
            self.state.simulated_balance += pnl;
            self.state.reset();
            println!("[SIM] Emergency exit at ${:.4}", sell_price);
        }
    }

    pub async fn sign_and_submit(&mut self, market: &Market, price: f64, size: f64, side: Side) -> bool {
         if let (Some(ref client), Some(ref signer)) = (&self.client, &self.signer_instance) {
             // Securely format to exactly 2 decimal places without floating-point drift
             let price_str = format!("{:.2}", price);
             let size_str = format!("{:.2}", size);
             let price_dec = Decimal::from_str(&price_str).unwrap_or(Decimal::ZERO);
             let size_dec = Decimal::from_str(&size_str).unwrap_or(Decimal::ZERO);

             if price_dec <= Decimal::ZERO || size_dec <= Decimal::ZERO { return false; }

             let order_builder = client.limit_order()
                .token_id(&market.id)
                .price(price_dec)
                .size(size_dec)
                .side(side);

            if let Ok(order) = order_builder.build().await {
                if let Ok(signed_order) = client.sign(signer, order).await {
                    if let Ok(resp) = client.post_order(signed_order).await {
                        println!("[PROD] Order Submitted! ID: {:?}", resp.order_id);
                        return true;
                    }
                }
            }
        }
        false
    }

    pub async fn reconcile_with_chain(&mut self) {
        // Wrap the entire RPC block in a 500ms timeout so a lagging node never freezes the 1ms tick loop.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            // 1. Sync USDC Balance for Sizing
            if self.trading_enabled {
                if let Some(usdc_bal) = self.check_usdc_balance().await {
                    if (self.state.simulated_balance - usdc_bal).abs() > 0.05 {
                        println!("[RECON] Syncing Capital: USDC ${:.2}", usdc_bal);
                        self.state.simulated_balance = usdc_bal;
                        self.state.save();
                    }
                }
            }

            // 2. Sync Shares for State Machine
            if let Some(market_id) = self.state.active_market_id.clone() {
                let markets = self.polymarket_rx.borrow().clone();
                if let Some(market) = markets.get(&market_id) {
                    if let Some(on_chain_shares) = self.check_on_chain_position(market).await {
                        
                        let now_ms = Utc::now().timestamp_millis() + (self.time_offset * 1000);

                        if self.state.state == BotState::PendingBuy {
                            if on_chain_shares > 0.01 {
                                println!("[RECON] Buy confirmed on-chain.");
                                self.state.state = BotState::InPosition;
                                self.state.position_size = on_chain_shares;
                                self.state.pending_since = 0;
                                self.state.save();
                            } else if now_ms - self.state.pending_since > 5000 {
                                println!("[RECON] Buy failed/timeout. Reverting to Idle.");
                                self.state.reset();
                            }
                        } else if self.state.state == BotState::PendingSell {
                            if on_chain_shares < 0.01 {
                                println!("[RECON] Sell confirmed on-chain.");
                                let msg = format!("✅ WINNER!\nSuccessfully sold position.\nBalance is now: ${:.2}", self.state.simulated_balance);
                                Self::send_telegram_alert(&msg).await;
                                self.state.reset();
                            } else if now_ms - self.state.pending_since > 5000 {
                                println!("[RECON] Sell failed/timeout. Reverting to InPosition.");
                                self.state.state = BotState::InPosition;
                                self.state.position_size = on_chain_shares;
                                self.state.pending_since = 0;
                                self.state.save();
                            }
                        } else if (on_chain_shares - self.state.position_size).abs() > 0.01 {
                            // Drift catch-all ONLY executes if state is Idle or InPosition
                            println!("[RECON] Share drift detected. Engine: {:.2}, Chain: {:.2}", self.state.position_size, on_chain_shares);
                            self.state.position_size = on_chain_shares;
                            if on_chain_shares < 0.01 {
                                self.state.reset();
                            } else {
                                self.state.state = BotState::InPosition;
                                self.state.save();
                            }
                        }
                    }
                }
            }
        }).await;
    }

    async fn check_usdc_balance(&self) -> Option<f64> {
        let provider = self.rpc_provider.as_ref()?;
        let funder = self.funder.unwrap_or(Address::zero());
        if funder.is_zero() { return None; }

        let usdc_address = "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359".parse::<Address>().ok()?;
        let mut call_data = [0x70, 0xa0, 0x82, 0x31, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].to_vec();
        call_data.extend_from_slice(funder.as_bytes());
        
        let tx = TransactionRequest::new().to(usdc_address).data(call_data).from(funder);
        provider.call(&tx.into(), None).await.ok().map(|res| {
            let bal = U256::from_big_endian(&res);
            bal.as_u128() as f64 / 1_000_000.0
        })
    }

    async fn check_on_chain_position(&self, market: &Market) -> Option<f64> {
        let provider = self.rpc_provider.as_ref()?;
        let funder = self.funder.unwrap_or(Address::zero());
        if funder.is_zero() { return None; }

        let token_id = U256::from_dec_str(&market.id).ok()?;
        let ctf_address = "0x4D97d6599A46602052E175369CeBa61a5b8cae6a".parse::<Address>().ok()?;
        
        let mut data = vec![0x00, 0xfd, 0xd5, 0x8e];
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(funder.as_bytes());
        let mut id_bytes = [0u8; 32];
        token_id.to_big_endian(&mut id_bytes);
        data.extend_from_slice(&id_bytes);

        let tx = TransactionRequest::new().to(ctf_address).data(data);
        if let Ok(res) = provider.call(&tx.into(), None).await {
            let bal = U256::from_big_endian(&res);
            return Some(bal.as_u128() as f64 / 1_000_000.0);
        }
        None
    }

    pub async fn initialize_client(&mut self, private_key: &str) -> bool {
        let trimmed_key = private_key.trim();
        let signer = match LocalSigner::from_str(trimmed_key) {
            Ok(s) => s.with_chain_id(Some(POLYGON)),
            Err(e) => {
                eprintln!("[SDK] Invalid PRIVATE_KEY: {}. Check that it is a valid 64-char hex string.", e);
                return false;
            }
        };
        let client_builder = match ClobClient::new("https://clob.polymarket.com", ClobConfig::default()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[SDK] Failed to create ClobClient: {}", e);
                return false;
            }
        };
        
        let mut auth_builder = client_builder.authentication_builder(&signer);
        
        if self.signature_type == 2 {
            if let Some(funder_addr) = self.funder {
                let sdk_addr = polymarket_client_sdk::types::Address::from_slice(funder_addr.as_bytes());
                auth_builder = auth_builder.funder(sdk_addr);
            }
            auth_builder = auth_builder.signature_type(polymarket_client_sdk::clob::types::SignatureType::GnosisSafe);
        }

        match auth_builder.authenticate().await {
            Ok(client) => {
                self.client = Some(client);
                self.signer_instance = Some(signer);
                true
            }
            Err(e) => {
                eprintln!("[SDK] Auth failed: {}", e);
                false
            }
        }
    }

    pub async fn grant_sell_permission() -> bool {
        let rpc_url = std::env::var("POLYGON_RPC_URL").expect("Missing POLYGON_RPC_URL");
        let priv_key = std::env::var("PRIVATE_KEY").expect("Missing PRIVATE_KEY");

        let provider = Provider::<Http>::try_from(rpc_url.as_str()).unwrap();
        let wallet = priv_key.parse::<LocalWallet>().unwrap().with_chain_id(137u64);
        
        let my_address = wallet.address();
        let client = std::sync::Arc::new(SignerMiddleware::new(provider, wallet));

        let ctf_address = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045".parse::<Address>().unwrap();
        let operator_address = "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E".parse::<Address>().unwrap();

        println!("[AUTH] Granting Sell Permission (setApprovalForAll) for: {:?}", my_address);

        let mut data = vec![0xa2, 0x2c, 0xb4, 0x65]; // Selector for setApprovalForAll
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(operator_address.as_bytes());
        data.extend_from_slice(&[0u8; 31]);
        data.push(1); 

        let tx = TransactionRequest::new()
            .to(ctf_address)
            .data(data)
            .from(my_address); 
        
        // Extract tx_hash immediately so PendingTransaction (which borrows client) is dropped
        let tx_result = client.send_transaction(tx, None).await
            .map(|pending_tx| {
                let hash = pending_tx.tx_hash();
                hash
            });

        match tx_result {
            Ok(hash) => {
                println!("[SUCCESS] Permission Transaction Sent: {:?}", hash);
                true
            }
            Err(e) => {
                eprintln!("[FAIL] Could not grant permission: {}", e);
                false
            }
        }
    }
}
