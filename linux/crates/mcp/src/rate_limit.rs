use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    max_per_minute: u32,
    windows: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        Self {
            max_per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, key: &str) -> Result<(), String> {
        let mut map = self.windows.lock().expect("rate limiter lock");
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let entries = map.entry(key.to_string()).or_default();
        entries.retain(|t| now.duration_since(*t) < window);
        if entries.len() as u32 >= self.max_per_minute {
            return Err(format!(
                "rate limit exceeded: {} requests / minute",
                self.max_per_minute
            ));
        }
        entries.push(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_under_limit() {
        let lim = RateLimiter::new(2);
        assert!(lim.check("a").is_ok());
        assert!(lim.check("a").is_ok());
        assert!(lim.check("a").is_err());
    }
}
