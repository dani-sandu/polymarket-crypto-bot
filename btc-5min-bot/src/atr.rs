use serde::Deserialize;
use std::collections::VecDeque;
use std::time::Instant;

#[derive(Debug, Deserialize)]
struct BinanceKline(
    i64,    // Open time
    String, // Open
    String, // High
    String, // Low
    String, // Close
    String, // Volume
    i64,    // Close time
    String, // Quote asset volume
    i64,    // Number of trades
    String, // Taker buy base asset volume
    String, // Taker buy quote asset volume
    String, // Ignore
);

#[derive(Debug, Clone)]
struct Candle {
    timestamp: i64,
    high: f64,
    low: f64,
    close: f64,
}

pub struct AtrMonitor {
    periods: usize,
    history: VecDeque<f64>, // Stores True Ranges
    current_candle: Option<Candle>,
    last_close: Option<f64>,
}

impl AtrMonitor {
    /// Bootstraps the ATR monitor using Binance REST API
    pub async fn new() -> Self {
        let mut monitor = Self {
            periods: 12,
            history: VecDeque::with_capacity(12),
            current_candle: None,
            last_close: None,
        };

        if let Err(e) = monitor.bootstrap().await {
            eprintln!("[ATR] Bootstrap failed: {}. Defaulting to safe high-volatility.", e);
            // Default to safe state: 12 high TRs so we don't think it's a desert
            for _ in 0..12 {
                monitor.history.push_back(1000.0);
            }
        }

        monitor
    }

    async fn bootstrap(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::new();
        // Fetch last 13 klines to get 12 closed TRs (needs prev close of first)
        let url = "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=5m&limit=13";
        let resp = client.get(url).send().await?.json::<Vec<serde_json::Value>>().await?;

        if resp.len() < 2 {
            return Err("Not enough klines for bootstrap".into());
        }

        for (i, k) in resp.iter().enumerate() {
            let close = k[4].as_str().unwrap_or("0").parse::<f64>()?;
            let high = k[2].as_str().unwrap_or("0").parse::<f64>()?;
            let low = k[3].as_str().unwrap_or("0").parse::<f64>()?;

            if i == 0 {
                self.last_close = Some(close);
                continue;
            }

            let tr = self.calculate_tr(high, low, self.last_close.unwrap());
            self.history.push_back(tr);
            self.last_close = Some(close);
        }

        // Keep only last 12
        while self.history.len() > self.periods {
            self.history.pop_front();
        }

        println!("[ATR] Bootstrap complete. Current ATR: ${:.2}", self.current_atr());
        Ok(())
    }

    fn calculate_tr(&self, high: f64, low: f64, prev_close: f64) -> f64 {
        let hl = high - low;
        let h_pc = (high - prev_close).abs();
        let l_pc = (low - prev_close).abs();
        hl.max(h_pc).max(l_pc)
    }

    /// Updates the monitor with a new price tick
    pub fn update_from_tick(&mut self, current_price: f64, current_timestamp: i64) {
        // Binance timestamps are in ms, but user requested i64 (assume ms for consistency)
        let candle_timestamp = (current_timestamp / 300_000) * 300_000;

        match &mut self.current_candle {
            Some(ref mut candle) if candle.timestamp == candle_timestamp => {
                // Update current candle
                if current_price > candle.high { candle.high = current_price; }
                if current_price < candle.low { candle.low = current_price; }
                candle.close = current_price;
            }
            _ => {
                // Period rollover
                if let Some(candle) = self.current_candle.take() {
                    if let Some(prev_close) = self.last_close {
                        let tr = self.calculate_tr(candle.high, candle.low, prev_close);
                        self.history.push_back(tr);
                        if self.history.len() > self.periods {
                            self.history.pop_front();
                        }
                    }
                    self.last_close = Some(candle.close);
                }

                // Initialize new candle
                self.current_candle = Some(Candle {
                    timestamp: candle_timestamp,
                    high: current_price,
                    low: current_price,
                    close: current_price,
                });
            }
        }
    }

    pub fn current_atr(&self) -> f64 {
        if self.history.is_empty() {
            return 1000.0; // Safe default
        }
        let sum: f64 = self.history.iter().sum();
        sum / self.history.len() as f64
    }

    pub fn is_volatility_desert(&self, threshold: f64) -> bool {
        self.current_atr() < threshold
    }
}
