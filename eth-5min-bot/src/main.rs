use ethers::prelude::*;
use ethers::signers::Signer as _;
use core_shared::{Market, TokenDirection};
use chrono::{Utc, DateTime, Timelike};
// use dotenv::dotenv;
use dotenv;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::env;
use std::fs;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;

mod strategy;
mod state;
mod atr;
mod velocity;

use crate::strategy::StrategyEngine;
use tokio::sync::watch;
mod config;
use config::MarketConfig;

// Persistent state now handled by crate::state::PersistentState

async fn fetch_active_market_id(asset: &str) -> Result<(String, String, i64, f64, f64), Box<dyn std::error::Error + Send + Sync>> {
    println!("Fetching active {} market ID...", asset.to_uppercase());
    let now = Utc::now();
    let minute = now.minute();

    // ETH & BTC both use 5m markets
    let (interval_secs, bucket_size_min, kline_interval, binance_symbol) = match asset {
        "eth" => (300u64, 5u32, "5m", "ETHUSDT"),
        "sol" => (900u64, 15u32, "15m", "SOLUSDT"),
        "xrp" => (900u64, 15u32, "15m", "XRPUSDT"),
        _     => (300u64, 5u32,  "5m",  "ETHUSDT"),
    };

    let current_bucket_minute = (minute / bucket_size_min) * bucket_size_min;
    let current_bucket_start = now
        .with_minute(current_bucket_minute)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_nanosecond(0)
        .unwrap();

    // Check current bucket and next bucket
    let timestamps_to_check = vec![
        current_bucket_start.timestamp(),
        current_bucket_start.timestamp() + interval_secs as i64,
    ];

    let client = reqwest::Client::new();
    for ts in timestamps_to_check {
        let slug = format!("{}-updown-5m-{}", asset, ts);
        let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
        
        println!("Checking slug: {}", slug);
        let resp = client.get(&url).send().await?;
        
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
             if let Some(event) = json.as_array().and_then(|arr| arr.first()) {
                if let Some(markets) = event["markets"].as_array() {
                     if let Some(market) = markets.first() {
                         let expiration = market["endDate"].as_str()
                            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.timestamp())
                            .unwrap_or(0);

                         let kline_url = format!("https://api.binance.com/api/v3/klines?symbol={}&interval={}&startTime={}&limit=1", binance_symbol, kline_interval, ts * 1000);
                         let k_resp = client.get(&kline_url).send().await?;
                         let strike_price = if k_resp.status().is_success() {
                             let k_json: serde_json::Value = k_resp.json().await?;
                             if let Some(kline) = k_json.as_array().and_then(|arr| arr.first()) {
                                 kline[1].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0)
                             } else {
                                 0.0
                             }
                         } else {
                             0.0
                         };

                         if let (Some(tokens_str), Some(outcomes_str)) = (market["clobTokenIds"].as_str(), market["outcomes"].as_str()) {
                             let tokens: Vec<String> = serde_json::from_str(tokens_str)?;
                             let outcomes: Vec<String> = serde_json::from_str(outcomes_str)?;
                             
                             let mut up_token = "".to_string();
                             let mut down_token = "".to_string();

                             for (i, outcome) in outcomes.iter().enumerate() {
                                 if outcome.eq_ignore_ascii_case("UP") || outcome.eq_ignore_ascii_case("YES") {
                                     if let Some(t) = tokens.get(i) { up_token = t.clone(); }
                                 } else if outcome.eq_ignore_ascii_case("DOWN") || outcome.eq_ignore_ascii_case("NO") {
                                     if let Some(t) = tokens.get(i) { down_token = t.clone(); }
                                 }
                             }
                             
                             if !up_token.is_empty() && !down_token.is_empty() && expiration > Utc::now().timestamp() {
                                 println!("[SUCCESS] {} {}m Markets Found: UP: {} | DOWN: {}", asset.to_uppercase(), bucket_size_min, up_token, down_token);
                                 
                                 let last_trade = market["lastTradePrice"].as_f64().unwrap_or(0.0);
                                 let initial_price = if last_trade > 0.0 { last_trade } else { 0.50 };

                                 return Ok((up_token, down_token, expiration, strike_price, initial_price));
                             }
                         }
                     }
                }
            }
        }
    }

    Err(format!("No active {} {}m market found", asset.to_uppercase(), bucket_size_min).into())
}

async fn fetch_server_time() -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    let resp = client.get("https://clob.polymarket.com/time").send().await?;
    let json: serde_json::Value = resp.json().await?;
    // Polymarket returns { "timestamp": 1234567890, ... }
    if let Some(ts) = json["timestamp"].as_i64() {
        Ok(ts)
    } else {
        Err("Invalid time response".into())
    }
}

#[tokio::main]
async fn main() {
    // dotenv().ok();
    println!("[ENV] Current CWD: {:?}", std::env::current_dir().unwrap_or_default());
    match dotenv::from_filename(".env.eth") {
        Ok(path) => println!("[ENV] Successfully loaded .env.eth from {:?}", path),
        Err(e) => {
            eprintln!("[ENV] FAILED to load .env.eth in CWD: {}", e);
            // Fallback: check parent directory (useful if running from target/release)
            match dotenv::from_filename("../.env.eth") {
                Ok(path) => println!("[ENV] Successfully loaded .env.eth from parent: {:?}", path),
                Err(_) => eprintln!("[ENV] FAILED to load .env.eth from parent directory."),
            }
        }
    }

    let market_config = MarketConfig::default_eth();

    // --- SINGLETON LOCK GUARD ---
    let lock_file = ".bot.lock";
    if fs::metadata(lock_file).is_ok() {
        eprintln!("[CRITICAL] LOCK FILE DETECTED ({}). ANOTHER BOT IS RUNNING OR CRASHED.", lock_file);
        eprintln!("To force start: rm {}", lock_file);
        std::process::exit(1);
    }
    let _ = fs::write(lock_file, Utc::now().to_rfc3339());
    env_logger::init();

    // MARKET_ASSET: "eth" (default)
    let market_asset = env::var("MARKET_ASSET")
        .unwrap_or_else(|_| "eth".to_string())
        .to_lowercase();

    // Derive Binance WS URL from asset if not explicitly set
    let binance_ws_url = env::var("BINANCE_WS_URL").unwrap_or_else(|_| {
        match market_asset.as_str() {
            "eth" => "wss://stream.binance.com:9443/ws/ethusdt@aggTrade".to_string(),
            _     => "wss://stream.binance.com:9443/ws/btcusdt@aggTrade".to_string(),
        }
    });
    let polymarket_ws_url = env::var("POLYMARKET_WS_URL").expect("POLYMARKET_WS_URL must be set");
    let trading_enabled =
        env::var("TRADING_ENABLED").unwrap_or_else(|_| "false".to_string()) == "true";

    let _private_key = env::var("PRIVATE_KEY").ok().map(|s| s.trim().to_string());
    let signature_type = env::var("SIGNATURE_TYPE")
        .unwrap_or_else(|_| "0".to_string())
        .parse::<u8>()
        .unwrap_or(0);
    let funder_address = env::var("FUNDER_ADDRESS")
        .ok()
        .and_then(|s| s.parse::<Address>().ok());
    
    // We are supporting both single-wallet and Proxy (Gnosis Safe) setups.
    if trading_enabled {
        println!("Trading ENABLED. Verifying credentials...");
        if _private_key.is_none() {
             eprintln!("WARNING: TRADING_ENABLED but PRIVATE_KEY missing!");
        }
    }

    println!("Starting PolyRustBot [{}] ... Trading Enabled: {}", market_asset.to_uppercase(), trading_enabled);

    // --- ZOMBIE KILLER: Kill any other running instances of the bot ---
    if trading_enabled {
        println!("[SAFETY] Ensuring no duplicate bits are running (pkill)...");
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg("pkill -9 poly_rust_bot")
            .spawn();
        sleep(Duration::from_millis(500)).await;
    }

    // --- DATA BROADCAST CHANNELS (watch) ---
    let (binance_tx, mut binance_rx) = watch::channel(0.0);
    let (coinbase_tx, coinbase_rx) = watch::channel(0.0);
    let (dvol_tx, dvol_rx) = watch::channel(0.0);
    let (polymarket_tx, polymarket_rx) = watch::channel(HashMap::<String, Market>::new());
    let (market_id_tx, market_id_rx) = watch::channel(None);

    // --- BINANCE WS TASK (Primary) ---
    let binance_ws_url_clone = binance_ws_url.clone();
    tokio::spawn(async move {
        loop {
            match connect_async(Url::parse(&binance_ws_url_clone).unwrap()).await {
                Ok((ws_stream, _)) => {
                    println!("[FEED] Connected to Binance WS");
                    let (_, mut read) = ws_stream.split();
                    while let Some(msg) = read.next().await {
                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Some(p_str) = json["p"].as_str() {
                                    if let Ok(price) = p_str.parse::<f64>() {
                                        let _ = binance_tx.send(price);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[FEED] Binance WS Error: {}. Reconnecting...", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // --- COINBASE WS TASK (Whale Alarm) ---
    let coinbase_ws_url = env::var("COINBASE_WS_URL")
        .unwrap_or_else(|_| "wss://ws-feed.exchange.coinbase.com".to_string());
    tokio::spawn(async move {
        loop {
            match connect_async(Url::parse(&coinbase_ws_url).unwrap()).await {
                Ok((mut ws_stream, _)) => {
                    println!("[FEED] Connected to Coinbase WS");
                    let sub = serde_json::json!({
                        "type": "subscribe",
                        "product_ids": ["ETH-USD"],
                        "channels": ["ticker"]
                    });
                    let _ = ws_stream.send(Message::Text(sub.to_string())).await;
                    let (_, mut read) = ws_stream.split();
                    while let Some(msg) = read.next().await {
                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Some(p_str) = json["price"].as_str() {
                                    if let Ok(price) = p_str.parse::<f64>() {
                                        let _ = coinbase_tx.send(price);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[FEED] Coinbase WS Error: {}. Reconnecting...", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // --- DERIBIT DVOL TASK (Macro Filter) ---
    tokio::spawn(async move {
        loop {
            match connect_async(Url::parse("wss://www.deribit.com/ws/api/v2").unwrap()).await {
                Ok((mut ws_stream, _)) => {
                    println!("[FEED] Connected to Deribit DVOL WS");
                    let subscribe_msg = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "public/subscribe",
                        "id": 1,
                        "params": {
                            "channels": ["deribit_volatility_index.eth_usd"]
                        }
                    });
                    let _ = ws_stream.send(Message::Text(subscribe_msg.to_string())).await;

                    let (_, mut read) = ws_stream.split();
                    while let Some(msg) = read.next().await {
                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if json["method"] == "subscription" {
                                    if let Some(val) = json["params"]["data"]["volatility"].as_f64() {
                                        let _ = dvol_tx.send(val);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[FEED] Deribit WS Error: {}. Reconnecting...", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // --- POLYMARKET WS TASK (Orderbook) ---
    let polymarket_ws_url_clone = polymarket_ws_url.clone();
    let market_asset_clone = market_asset.clone();
    tokio::spawn(async move {
        loop {
            let (up_id, down_id, exp, strike, initial_price);
            match fetch_active_market_id(&market_asset_clone).await {
                Ok((u, d, e, s, p)) => {
                    up_id = u; down_id = d; exp = e; strike = s; initial_price = p;
                    let _ = market_id_tx.send(Some(up_id.clone()));
                }
                Err(e) => {
                    eprintln!("[FEED] Polymarket meta error: {}. Retrying...", e);
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }

            match connect_async(Url::parse(&polymarket_ws_url_clone).unwrap()).await {
                Ok((mut ws_stream, _)) => {
                    println!("[FEED] Connected to Polymarket WS");
                    let sub = serde_json::json!({"assets_ids": [up_id, down_id], "type": "market"});
                    let _ = ws_stream.send(Message::Text(sub.to_string())).await;

                    let mut local_markets = HashMap::new();
                    let mut m_up = Market::new(up_id.clone(), TokenDirection::Up, exp, strike);
                    m_up.last_price = initial_price;
                    local_markets.insert(up_id.clone(), m_up);

                    let mut m_down = Market::new(down_id.clone(), TokenDirection::Down, exp, strike);
                    m_down.last_price = 1.0 - initial_price;
                    local_markets.insert(down_id.clone(), m_down);
                    
                    let _ = polymarket_tx.send(local_markets.clone());

                    let (_, mut read) = ws_stream.split();
                    while let Some(msg) = tokio::time::timeout(Duration::from_secs(30), read.next()).await.unwrap_or(None) {
                        if Utc::now().timestamp() > exp + 2 { break; } // Candle expired

                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                let mut changed = false;
                                let updates = if let Some(changes) = json["price_changes"].as_array() {
                                    changes.iter().filter_map(|c| {
                                        let id = c["asset_id"].as_str().or(c["market"].as_str())?;
                                        Some((id.to_string(), c.clone()))
                                    }).collect::<Vec<_>>()
                                } else if let Some(id) = json["asset_id"].as_str().or(json["market"].as_str()) {
                                    vec![(id.to_string(), json.clone())]
                                } else { vec![] };

                                for (id, data) in updates {
                                    if let Some(m) = local_markets.get_mut(&id) {
                                        let bids = data["bids"].as_array().map(|arr| arr.iter().filter_map(|x| Some((x["price"].as_str()?.to_string(), x["size"].as_str()?.to_string()))).collect());
                                        let asks = data["asks"].as_array().map(|arr| arr.iter().filter_map(|x| Some((x["price"].as_str()?.to_string(), x["size"].as_str()?.to_string()))).collect());
                                        m.orderbook.update(bids, asks);
                                        changed = true;
                                    }
                                }
                                if changed { let _ = polymarket_tx.send(local_markets.clone()); }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[FEED] Polymarket WS error: {}. Reconnecting...", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });

    // Signer creation moved to StrategyEngine::initialize_client using SDK

    // Main Strategy Loop
    // --- TIME SYNCHRONIZATION (Fix for 2026 vs 2025 skew) ---
    // User's system clock is 1 year ahead. We must verify server time.
    let time_offset = match fetch_server_time().await {
        Ok(server_ts) => {
            let local_ts = Utc::now().timestamp();
            let offset = server_ts - local_ts;
            println!("[TIME] Server: {} | Local: {} | Offset: {}s", server_ts, local_ts, offset);
            if offset.abs() > 300 {
                println!("[WARN] Large time skew detected! Adjusting all auth timestamps by {}s", offset);
            }
            offset
        },
        Err(e) => {
            eprintln!("[WARN] Failed to sync time: {}. Defaulting to 0 offset.", e);
            0
        }
    };


    // --- INITIALIZE REAL BALANCE IF TRADING ---
    let mut initial_bal = 3.0; // Reset to $3 for simulation based on user request
    if trading_enabled {
        if let (Ok(rpc_url), Some(pk)) = (env::var("POLYGON_RPC_URL"), &_private_key) {
            if let Ok(provider) = Provider::<Http>::try_from(rpc_url) {
                if let Ok(wallet) = pk.parse::<LocalWallet>() {
                    let client_middleware = SignerMiddleware::new(provider, wallet.with_chain_id(137u64));
                    let usdc_address = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174".parse::<Address>().unwrap();
                    
                    let target_addr = funder_address.unwrap_or(client_middleware.address());
                    
                    // Call USDC balanceOf
                    let data = [
                        0x70, 0xa0, 0x82, 0x31, // selector for balanceOf(address)
                        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    ].to_vec();
                    
                    let mut call_data = data;
                    call_data.extend_from_slice(target_addr.as_bytes());
                    
                    let tx = TransactionRequest::new()
                        .to(usdc_address)
                        .data(call_data);
                    
                    if let Ok(res) = client_middleware.call(&tx.into(), None).await {
                        let bal = U256::from_big_endian(&res);
                        initial_bal = bal.as_u128() as f64 / 1_000_000.0;
                        println!("[AUTH] Synced Real Balance: ${:.2}", initial_bal);
                    }
                }
            }
        }
    }

    // --- ONE-TIME SETUP TRIGGER ---
    if std::env::var("SETUP_PERMISSIONS").unwrap_or_default() == "true" {
        println!("\n============================================");
        println!("[SETUP MODE] Initializing Blockchain Permissions...");
        
        strategy::StrategyEngine::grant_sell_permission().await;
        
        println!("[SETUP MODE] Complete! Please wait 30 seconds for Polygon to confirm.");
        println!("Then, restart the bot WITHOUT the SETUP_PERMISSIONS flag.");
        println!("============================================\n");
        // Remove the lock file so we can restart without 'rm .bot.lock'
        let _ = fs::remove_file(lock_file);
        std::process::exit(0);
    }

    let mut strategy = StrategyEngine::new(
        trading_enabled,
        market_asset.clone(),
        signature_type,
        funder_address,
        time_offset,
        binance_rx,
        coinbase_rx,
        dvol_rx,
        polymarket_rx,
        market_id_rx,
    ).await;

    // ------------------------------

    // --- REAL BALANCE PRIORITY ---
    // If live trading is enabled, the blockchain is the single source of truth for capital.
    if trading_enabled {
        strategy.state.simulated_balance = initial_bal;
        println!("[SAFETY] Live Mode detected. Overriding memory with real wallet balance: ${:.2}", initial_bal);
    }

    // Initialize SDK
    if trading_enabled {
        if let Some(pk) = _private_key {
            if strategy.initialize_client(&pk).await {
                println!("[AUTH] Polymarket SDK Authenticated.");
            }
        }
    }

    // --- GRACEFUL SHUTDOWN ---
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();

    // --- DIAGNOSTIC GHOST TEST TRIGGER ---
    if std::env::var("RUN_GHOST_TEST").unwrap_or_default() == "true" {
        println!("[SETUP] Waiting 3 seconds for WebSocket feeds to populate Market IDs...");
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        strategy.run_ghost_test().await;
        std::process::exit(0); // Safely exit without entering the trading loop
    }
    // -------------------------------------

    println!("[SNIPER] High-Frequency Loop Active. Priority: 1ms.");

    tokio::select! {
        _ = async {
            loop {
                // Core Tick Pulse
                let b_price = *strategy.binance_rx.borrow();
                let c_price = *strategy.coinbase_rx.borrow();
                strategy.execute_tick(b_price, c_price, &market_config).await;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        } => {},
        _ = tokio::signal::ctrl_c() => {
            println!("\n[SHUTDOWN] SIGINT received.");
            strategy.state.save();
        },
        _ = sigterm.recv() => {
            println!("\n[SHUTDOWN] SIGTERM received.");
            strategy.state.save();
        }
    }

    let _ = fs::remove_file(".bot.lock");
}
