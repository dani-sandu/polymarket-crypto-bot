use serde::{Serialize, Deserialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum BotState {
    Idle,
    PendingBuy,
    InPosition,
    PendingSell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentState {
    pub state: BotState,
    pub active_market_id: Option<String>,
    pub entry_price: f64,
    pub position_size: f64,
    pub vault: f64,
    pub cumulative_pnl: f64,
    pub simulated_balance: f64,
    pub pending_since: i64,
}

impl Default for PersistentState {
    fn default() -> Self {
        Self {
            state: BotState::Idle,
            active_market_id: None,
            entry_price: 0.0,
            position_size: 0.0,
            vault: 0.0,
            cumulative_pnl: 0.0,
            simulated_balance: 3.0, // Default starting balance
            pending_since: 0,
        }
    }
}

impl PersistentState {
    pub fn load() -> Self {
        if Path::new("state.json").exists() {
            if let Ok(data) = fs::read_to_string("state.json") {
                if let Ok(state) = serde_json::from_str(&data) {
                    println!("[STATE] Loaded persistent state from state.json");
                    return state;
                }
            }
        }
        println!("[STATE] No previous state found. Initializing new state.");
        Self::default()
    }

    /// Reset the state machine to Idle, clearing position data.
    /// Does NOT reset simulated_balance, vault, or cumulative_pnl.
    pub fn reset(&mut self) {
        self.state = BotState::Idle;
        self.active_market_id = None;
        self.entry_price = 0.0;
        self.position_size = 0.0;
        self.pending_since = 0;
        self.save();
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Err(e) = fs::write("state.json", json) {
                eprintln!("[STATE] Failed to save state.json: {}", e);
            }
        }
    }
}
