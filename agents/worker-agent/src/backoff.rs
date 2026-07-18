use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Backoff {
    initial: Duration,
    maximum: Duration,
    ceiling: Duration,
}

impl Backoff {
    pub fn new(initial: Duration, maximum: Duration) -> Self {
        Self {
            initial,
            maximum,
            ceiling: initial,
        }
    }

    /// Full jitter bounded away from zero to prevent a reconnect spin.
    pub fn next_delay(&mut self) -> Duration {
        let max_millis = self.ceiling.as_millis().max(1).min(u64::MAX as u128) as u64;
        let millis = rand::random_range(1..=max_millis);
        self.ceiling = self.ceiling.saturating_mul(2).min(self.maximum);
        Duration::from_millis(millis)
    }

    pub fn reset(&mut self) {
        self.ceiling = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_ceiling_is_capped_and_resettable() {
        let mut backoff = Backoff::new(Duration::from_millis(10), Duration::from_millis(20));
        assert!(backoff.next_delay() <= Duration::from_millis(10));
        assert!(backoff.next_delay() <= Duration::from_millis(20));
        assert!(backoff.next_delay() <= Duration::from_millis(20));
        backoff.reset();
        assert!(backoff.next_delay() <= Duration::from_millis(10));
    }
}
