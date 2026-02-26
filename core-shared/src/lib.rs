use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBook {
    pub bids: Vec<(String, String)>, // Price, Size (Strings to avoid precision issues in raw json)
    pub asks: Vec<(String, String)>,
}

#[allow(dead_code)]
impl Default for OrderBook {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: Vec::new(),
            asks: Vec::new(),
        }
    }

    // Helper to get best bid/ask as f64
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().and_then(|(p, _)| p.parse::<f64>().ok())
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().and_then(|(p, _)| p.parse::<f64>().ok())
    }

    pub fn is_valid(&self) -> bool {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => b < a,
            _ => true,
        }
    }
    
    // Update processes deltas properly
    pub fn update(&mut self, bids: Option<Vec<(String, String)>>, asks: Option<Vec<(String, String)>>) {
        if let Some(new_bids) = bids {
            for (price, size) in new_bids {

                if size == "0" {
                    self.bids.retain(|(p, _)| p != &price);
                } else if let Some(pos) = self.bids.iter().position(|(p, _)| p == &price) {
                    self.bids[pos] = (price, size);
                } else {
                    self.bids.push((price, size));
                }
            }
            // Sort bids descending
            self.bids.sort_by(|(p1, _), (p2, _)| {
                let p1_f = p1.parse::<f64>().unwrap_or(0.0);
                let p2_f = p2.parse::<f64>().unwrap_or(0.0);
                p2_f.partial_cmp(&p1_f).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        if let Some(new_asks) = asks {
            for (price, size) in new_asks {
                if size == "0" {
                    self.asks.retain(|(p, _)| p != &price);
                } else if let Some(pos) = self.asks.iter().position(|(p, _)| p == &price) {
                    self.asks[pos] = (price, size);
                } else {
                    self.asks.push((price, size));
                }
            }
            // Sort asks ascending
            self.asks.sort_by(|(p1, _), (p2, _)| {
                let p1_f = p1.parse::<f64>().unwrap_or(1.0);
                let p2_f = p2.parse::<f64>().unwrap_or(1.0);
                p1_f.partial_cmp(&p2_f).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenDirection {
    Up,
    Down,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Market {
    pub id: String,
    pub direction: TokenDirection,
    pub orderbook: OrderBook,
    pub last_price: f64,
    pub expiration: i64, // Unix timestamp
    pub strike_price: f64, // The "Open" price of the 5m candle
}

#[allow(dead_code)]
impl Market {
    pub fn new(id: String, direction: TokenDirection, expiration: i64, strike_price: f64) -> Self {
        Self {
            id,
            direction,
            orderbook: OrderBook::new(),
            last_price: 0.0,
            expiration,
            strike_price,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct Trade {
    pub price: f64,
    pub size: f64,
    pub side: String, // "BUY" or "SELL"
    pub timestamp: i64,
}
