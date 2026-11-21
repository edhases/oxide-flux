//! Payload-only token-bucket rate limiting.
//!
//! Two rules from the project contract drive the shape of this module:
//!
//! - only payload counts, so protocol framing, keep-alives and handshakes are
//!   never charged;
//! - LAN peers are never limited, which is a policy decision the caller makes
//!   per peer, not something the limiter can infer.
//!
//! Both fall out of the API: [`RateLimiter::try_consume`] charges exactly the
//! byte count it is given, and [`RateLimiter::try_consume_classified`] skips
//! the bucket entirely for [`TrafficClass::Overhead`] and [`TrafficClass::Local`].
//!
//! There is no clock in here either. Every call takes the caller's monotonic
//! `now`, so tests are exact and a single scheduler can drive several limiters
//! from one timestamp.
//!
//! A session owns one limiter per direction (upload and download); the type is
//! direction-agnostic and has no notion of peers, so per-peer fairness stays
//! with the actor that schedules requests.

use std::time::Duration;

/// Classification of traffic for rate limiting.
///
/// Only [`TrafficClass::Payload`] is charged. The classification exists so the
/// policy is written down once instead of at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficClass {
    /// User data: `piece` blocks and other byte counts the user asked for.
    Payload,
    /// Protocol and transport overhead: handshakes, keep-alives, ids, length
    /// prefixes, headers, TCP/IP framing.
    Overhead,
    /// Traffic to or from a peer on the local network.
    Local,
}

impl TrafficClass {
    /// Whether this class consumes tokens.
    #[must_use]
    pub const fn counts_towards_limit(self) -> bool {
        matches!(self, Self::Payload)
    }
}

/// A token bucket measured in bytes.
///
/// The bucket starts full (one `burst`), refills at `rate` bytes per second, and
/// never holds more than `burst` bytes. A rate of zero means zero throughput:
/// the bucket is emptied and stays empty, even if tokens were left over.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    rate: u64,
    burst: u64,
    tokens: f64,
    last: Duration,
    charged: u64,
}

impl RateLimiter {
    /// Creates a limiter at `rate` bytes per second with a `burst`-byte cap.
    ///
    /// The bucket starts full unless `rate` is zero, in which case it starts
    /// empty because zero rate means blocked.
    #[must_use]
    pub fn new(rate: u64, burst: u64) -> Self {
        Self {
            rate,
            burst,
            tokens: if rate == 0 { 0.0 } else { burst as f64 },
            last: Duration::ZERO,
            charged: 0,
        }
    }

    /// The configured rate in bytes per second.
    #[must_use]
    pub fn rate(&self) -> u64 {
        self.rate
    }

    /// The configured burst cap in bytes.
    #[must_use]
    pub fn burst(&self) -> u64 {
        self.burst
    }

    /// Total payload bytes charged since construction.
    ///
    /// Overhead and LAN traffic never reach this counter.
    #[must_use]
    pub fn charged(&self) -> u64 {
        self.charged
    }

    /// Changes the rate without touching the bucket contents beyond the burst
    /// cap.
    ///
    /// Elapsed time is credited at the *old* rate, so a rate change cannot
    /// retroactively mint tokens. Setting the rate to zero empties the bucket.
    pub fn set_rate(&mut self, rate: u64, now: Duration) {
        self.refill(now);
        self.rate = rate;
        if rate == 0 {
            self.tokens = 0.0;
        } else {
            self.tokens = self.tokens.min(self.burst as f64);
        }
    }

    /// Changes the burst cap, clamping the current bucket to it.
    ///
    /// Elapsed time is credited under the *old* cap first, so raising the cap
    /// does not retroactively fill the bucket with bytes that were never
    /// allowed to accumulate.
    pub fn set_burst(&mut self, burst: u64, now: Duration) {
        self.refill(now);
        self.burst = burst;
        self.tokens = self.tokens.min(burst as f64);
    }

    /// Bytes available at `now`, rounded down.
    pub fn available(&mut self, now: Duration) -> u64 {
        self.refill(now);
        self.tokens.max(0.0).floor() as u64
    }

    /// Charges `payload_bytes` to the bucket if they fit.
    ///
    /// Returns `false` without charging anything when the bucket is short, so
    /// the caller can retry later; there is no partial debit and no debt.
    /// A zero-byte charge always succeeds.
    pub fn try_consume(&mut self, payload_bytes: u64, now: Duration) -> bool {
        self.refill(now);
        // A request bigger than the cap can never fit, however long we wait.
        if payload_bytes > self.burst {
            return false;
        }
        let needed = payload_bytes as f64;
        if self.tokens >= needed {
            self.tokens -= needed;
            self.charged = self.charged.saturating_add(payload_bytes);
            true
        } else {
            false
        }
    }

    /// Charges only payload traffic; overhead and LAN traffic are free.
    pub fn try_consume_classified(
        &mut self,
        class: TrafficClass,
        bytes: u64,
        now: Duration,
    ) -> bool {
        if class.counts_towards_limit() {
            self.try_consume(bytes, now)
        } else {
            true
        }
    }

    /// How long until `payload_bytes` would fit, or `None` if it never will
    /// (zero rate, or a request larger than the burst cap).
    pub fn time_until(&mut self, payload_bytes: u64, now: Duration) -> Option<Duration> {
        self.refill(now);
        let needed = payload_bytes as f64;
        if self.tokens >= needed {
            return Some(Duration::ZERO);
        }
        if self.rate == 0 || payload_bytes > self.burst {
            return None;
        }
        let missing = needed - self.tokens;
        Duration::try_from_secs_f64(missing / self.rate as f64).ok()
    }

    /// Credits elapsed time, treating a rewound clock as zero elapsed time.
    fn refill(&mut self, now: Duration) {
        let elapsed = now.saturating_sub(self.last);
        if elapsed > Duration::ZERO {
            self.last = now;
        }
        if self.rate == 0 {
            self.tokens = 0.0;
            return;
        }
        if elapsed.is_zero() {
            return;
        }
        let added = (elapsed.as_nanos() as f64) * (self.rate as f64) / 1_000_000_000.0;
        self.tokens = (self.tokens + added).min(self.burst as f64);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const ZERO: Duration = Duration::ZERO;

    #[test]
    fn starts_full_and_drains() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert_eq!(limiter.rate(), 1000);
        assert_eq!(limiter.burst(), 1000);
        assert_eq!(limiter.available(ZERO), 1000);
        assert!(limiter.try_consume(1000, ZERO));
        assert_eq!(limiter.available(ZERO), 0);
        assert!(!limiter.try_consume(1, ZERO));
        assert_eq!(limiter.charged(), 1000);
    }

    #[test]
    fn sustained_rate_is_byte_exact() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert!(limiter.try_consume(1000, ZERO));
        // 1 ms at 1 kB/s is exactly one byte; over 1000 ms the bucket supplies
        // exactly 1000 more, no more and no less.
        for ms in 1..=1000u64 {
            assert!(limiter.try_consume(1, Duration::from_millis(ms)), "ms {ms}");
        }
        assert!(!limiter.try_consume(1, Duration::from_millis(1000)));
        assert_eq!(limiter.charged(), 2000);
    }

    #[test]
    fn idle_time_never_exceeds_the_burst_cap() {
        let mut limiter = RateLimiter::new(1_000_000, 4096);
        assert_eq!(limiter.available(Duration::from_secs(3600)), 4096);
        assert!(limiter.try_consume(4096, Duration::from_secs(3600)));
        assert!(!limiter.try_consume(1, Duration::from_secs(3600)));
    }

    #[test]
    fn rate_change_uses_the_old_rate_for_elapsed_time() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert!(limiter.try_consume(1000, ZERO)); // empty at t=0
        assert!(limiter.try_consume(500, Duration::from_millis(500))); // 500 accrued

        limiter.set_rate(2000, Duration::from_millis(500));
        assert_eq!(limiter.available(Duration::from_millis(500)), 0);
        assert!(limiter.try_consume(1000, Duration::from_millis(1000))); // 500 ms * 2 kB/s
        assert!(!limiter.try_consume(1, Duration::from_millis(1000)));
    }

    #[test]
    fn lowering_the_rate_does_not_mint_a_burst() {
        let mut limiter = RateLimiter::new(10_000, 10_000);
        assert!(limiter.try_consume(1000, ZERO));
        limiter.set_rate(100, Duration::from_secs(10));
        // Old rate credited 10 s * 10 kB/s, but the burst cap still applies.
        assert_eq!(limiter.available(Duration::from_secs(10)), 10_000);
        assert!(limiter.try_consume(10_000, Duration::from_secs(10)));
        assert!(!limiter.try_consume(1, Duration::from_secs(10)));
    }

    #[test]
    fn burst_change_clamps_the_bucket() {
        let mut limiter = RateLimiter::new(1000, 1000);
        limiter.set_burst(100, ZERO);
        assert_eq!(limiter.available(ZERO), 100);
        assert!(limiter.try_consume(100, ZERO));
        // The elapsed half second was credited while the cap was still 100.
        limiter.set_burst(1000, Duration::from_millis(500));
        assert_eq!(limiter.available(Duration::from_millis(500)), 100);
        assert!(limiter.try_consume(100, Duration::from_millis(500)));
        // Under the larger cap the next half second accrues the full 500.
        assert_eq!(limiter.available(Duration::from_millis(1000)), 500);
    }

    #[test]
    fn zero_rate_is_blocked() {
        let mut limiter = RateLimiter::new(0, 1000);
        assert_eq!(limiter.available(ZERO), 0);
        assert!(!limiter.try_consume(1, ZERO));
        assert!(!limiter.try_consume(1, Duration::from_secs(60)));
        assert_eq!(limiter.time_until(1, ZERO), None);
        assert_eq!(limiter.charged(), 0);

        // Restoring the rate does not back-credit the blocked time.
        limiter.set_rate(1000, Duration::from_secs(60));
        assert_eq!(limiter.available(Duration::from_secs(60)), 0);
        assert!(limiter.try_consume(1000, Duration::from_secs(61)));
    }

    #[test]
    fn setting_rate_to_zero_drops_remaining_tokens() {
        let mut limiter = RateLimiter::new(1000, 1000);
        limiter.set_rate(0, ZERO);
        assert_eq!(limiter.available(ZERO), 0);
        assert!(!limiter.try_consume(1, ZERO));
    }

    #[test]
    fn zero_burst_blocks_everything() {
        let mut limiter = RateLimiter::new(1_000_000, 0);
        assert_eq!(limiter.available(Duration::from_secs(1)), 0);
        assert!(!limiter.try_consume(1, Duration::from_secs(1)));
        assert!(limiter.try_consume(0, Duration::from_secs(1)));
    }

    #[test]
    fn overhead_and_lan_traffic_are_never_counted() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert!(limiter.try_consume_classified(TrafficClass::Payload, 1000, ZERO));

        for _ in 0..10_000 {
            assert!(limiter.try_consume_classified(TrafficClass::Overhead, 1500, ZERO));
            assert!(limiter.try_consume_classified(TrafficClass::Local, 1_000_000, ZERO));
        }

        assert_eq!(limiter.available(ZERO), 0);
        assert_eq!(limiter.charged(), 1000);
        assert!(!limiter.try_consume_classified(TrafficClass::Payload, 1, ZERO));
    }

    #[test]
    fn traffic_classes_document_the_policy() {
        assert!(TrafficClass::Payload.counts_towards_limit());
        assert!(!TrafficClass::Overhead.counts_towards_limit());
        assert!(!TrafficClass::Local.counts_towards_limit());
    }

    #[test]
    fn zero_byte_charges_are_free() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert!(limiter.try_consume(0, ZERO));
        assert_eq!(limiter.available(ZERO), 1000);
        assert_eq!(limiter.charged(), 0);
    }

    #[test]
    fn rewound_clock_does_not_refill() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert!(limiter.try_consume(1000, Duration::from_secs(10)));
        assert!(!limiter.try_consume(1, Duration::from_secs(9)));
        assert!(limiter.try_consume(1, Duration::from_millis(10_001)));
    }

    #[test]
    fn a_request_larger_than_the_burst_can_never_succeed() {
        let mut limiter = RateLimiter::new(1_000_000, 1000);
        assert!(!limiter.try_consume(1001, Duration::from_secs(10)));
        assert_eq!(limiter.available(Duration::from_secs(10)), 1000);
        assert_eq!(limiter.charged(), 0);
    }

    #[test]
    fn time_until_reports_the_deficit() {
        let mut limiter = RateLimiter::new(1000, 1000);
        assert_eq!(limiter.time_until(100, ZERO), Some(ZERO));
        assert!(limiter.try_consume(1000, ZERO));
        assert_eq!(
            limiter.time_until(100, ZERO),
            Some(Duration::from_millis(100))
        );
        assert_eq!(limiter.time_until(1001, ZERO), None);
    }
}
