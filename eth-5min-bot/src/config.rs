pub struct MarketConfig {
    pub ticker: String,
    pub tier_1_gap: f64,
    pub tier_2_gap: f64,
    pub tier_3_gap: f64,
    pub killswitch_threshold: f64,
    pub max_spread: f64,
}

impl MarketConfig {
    pub fn default_eth() -> Self {
        Self {
            ticker: "ETH".to_string(),
            tier_1_gap: 15.0,
            tier_2_gap: 10.0,
            tier_3_gap: 7.0,
            killswitch_threshold: 4.0,
            max_spread: 0.02,
        }
    }
}
