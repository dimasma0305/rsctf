use std::time::Instant;

/// Per-session token buckets for authenticated control traffic. The wire
/// payload length comes from the framing layer, so byte admission adds no
/// serialization or allocation to the hot loop.
pub(crate) struct ControlAdmission {
    messages: TokenBucket,
    bytes: TokenBucket,
}

impl ControlAdmission {
    pub(crate) fn new(
        messages_per_second: usize,
        message_burst: usize,
        bytes_per_second: usize,
        byte_burst: usize,
    ) -> Self {
        Self::new_at(
            messages_per_second,
            message_burst,
            bytes_per_second,
            byte_burst,
            Instant::now(),
        )
    }

    fn new_at(
        messages_per_second: usize,
        message_burst: usize,
        bytes_per_second: usize,
        byte_burst: usize,
        now: Instant,
    ) -> Self {
        Self {
            messages: TokenBucket::new(messages_per_second, message_burst, now),
            bytes: TokenBucket::new(bytes_per_second, byte_burst, now),
        }
    }

    pub(crate) fn try_admit(&mut self, payload_bytes: usize) -> bool {
        self.try_admit_at(payload_bytes, Instant::now())
    }

    fn try_admit_at(&mut self, payload_bytes: usize, now: Instant) -> bool {
        self.messages.refill(now);
        self.bytes.refill(now);
        if !self.messages.can_take(1) || !self.bytes.can_take(payload_bytes) {
            return false;
        }
        self.messages.take(1);
        self.bytes.take(payload_bytes);
        true
    }
}

struct TokenBucket {
    refill_per_second: usize,
    capacity: usize,
    available: usize,
    fractional_nanos: u128,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(refill_per_second: usize, capacity: usize, now: Instant) -> Self {
        Self {
            refill_per_second,
            capacity,
            available: capacity,
            fractional_nanos: 0,
            last_refill: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.last_refill = now;
        if self.available == self.capacity {
            self.fractional_nanos = 0;
            return;
        }

        const NANOS_PER_SECOND: u128 = 1_000_000_000;
        let numerator = elapsed
            .as_nanos()
            .saturating_mul(self.refill_per_second as u128)
            .saturating_add(self.fractional_nanos);
        let generated = (numerator / NANOS_PER_SECOND).min(usize::MAX as u128) as usize;
        let missing = self.capacity - self.available;
        if generated >= missing {
            self.available = self.capacity;
            self.fractional_nanos = 0;
        } else {
            self.available += generated;
            self.fractional_nanos = numerator % NANOS_PER_SECOND;
        }
    }

    fn can_take(&self, amount: usize) -> bool {
        self.available >= amount
    }

    fn take(&mut self, amount: usize) {
        self.available -= amount;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn admission_is_atomic_and_refills_both_quotas() {
        let start = Instant::now();
        let mut admission = ControlAdmission::new_at(2, 2, 10, 10, start);
        assert!(admission.try_admit_at(5, start));
        assert!(admission.try_admit_at(5, start));
        assert!(!admission.try_admit_at(1, start));

        let half_second = start + Duration::from_millis(500);
        assert!(admission.try_admit_at(5, half_second));
        assert!(!admission.try_admit_at(1, half_second));
    }

    #[test]
    fn rejected_bytes_do_not_consume_a_message_token() {
        let start = Instant::now();
        let mut admission = ControlAdmission::new_at(1, 1, 10, 10, start);
        assert!(!admission.try_admit_at(11, start));
        assert!(admission.try_admit_at(10, start));
    }
}
