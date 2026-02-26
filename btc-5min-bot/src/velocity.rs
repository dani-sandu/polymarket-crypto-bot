use std::time::{Instant, Duration};

pub struct VelocityLockout {
    last_price: f64,
    last_update: Instant,
    lockout_until: Option<Instant>,
    threshold: f64,
    duration: Duration,
}

impl VelocityLockout {
    pub fn new(threshold: f64, duration_secs: u64) -> Self {
        Self {
            last_price: 0.0,
            last_update: Instant::now(),
            lockout_until: None,
            threshold,
            duration: Duration::from_secs(duration_secs),
        }
    }

    pub fn update(&mut self, current_price: f64) {
        let now = Instant::now();
        
        if self.last_price != 0.0 {
            let delta = (current_price - self.last_price).abs();
            let elapsed = now.duration_since(self.last_update);

            // If move > $30 in < 3 seconds
            if delta > self.threshold && elapsed < Duration::from_secs(3) {
                println!("[VELOCITY] Lockout triggered! Move: ${:.2} in {:.2}s", delta, elapsed.as_secs_f64());
                self.lockout_until = Some(now + self.duration);
            }
        }

        self.last_price = current_price;
        self.last_update = now;
    }

    pub fn is_locked(&self) -> bool {
        if let Some(until) = self.lockout_until {
            if Instant::now() < until {
                return true;
            }
        }
        false
    }
}
