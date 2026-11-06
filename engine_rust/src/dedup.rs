//! In-memory duplicate-signal suppression (zero round-trips).
//!
//! Keys are 64-bit FNV-1a hashes of `symbol + side`, so no `String` is
//! allocated per signal. The Redis lock (`processed_signal:*`) is still used
//! by the executor for cross-process/cross-restart safety, but this layer is
//! what stops the 3 redundant UDP packets and repeated channel edits.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::types::Side;

pub struct Deduplicator {
    seen: DashMap<u64, Instant>,
    ttl: Duration,
}

impl Deduplicator {
    pub fn new(ttl_secs: u64) -> Arc<Self> {
        Arc::new(Self {
            seen: DashMap::with_capacity(1024),
            ttl: Duration::from_secs(ttl_secs.max(1)),
        })
    }

    /// Returns `true` when the signal is new (and records it), `false` for a
    /// duplicate inside the TTL window.
    pub fn check_and_mark(&self, symbol: &str, side: Side) -> bool {
        let key = key(symbol, side);
        let now = Instant::now();
        match self.seen.entry(key) {
            Entry::Occupied(mut entry) => {
                if now.duration_since(*entry.get()) < self.ttl {
                    false
                } else {
                    entry.insert(now);
                    true
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        }
    }

    /// Drops the lock of the opposite side, so a real reversal is not blocked
    /// by the previous direction.
    pub fn remove_opposite(&self, symbol: &str, side: Side) {
        self.seen.remove(&key(symbol, side.opposite()));
    }

    /// Removes expired entries (call periodically; cheap `retain`).
    pub fn sweep(&self) {
        let ttl = self.ttl;
        self.seen.retain(|_, seen_at| seen_at.elapsed() < ttl);
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }
}

#[inline]
fn key(symbol: &str, side: Side) -> u64 {
    // FNV-1a, 64-bit.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for byte in symbol.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash ^= match side {
        Side::Buy => 0xB0,
        Side::Sell => 0x5E,
    };
    hash.wrapping_mul(PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_within_ttl_is_rejected() {
        let dedup = Deduplicator::new(60);
        assert!(dedup.check_and_mark("BTCUSDT", Side::Buy));
        assert!(!dedup.check_and_mark("BTCUSDT", Side::Buy));
        assert!(dedup.check_and_mark("BTCUSDT", Side::Sell));
        assert!(dedup.check_and_mark("ETHUSDT", Side::Buy));
        assert_eq!(dedup.len(), 3);
    }

    #[test]
    fn reversal_clears_the_opposite_lock() {
        let dedup = Deduplicator::new(60);
        assert!(dedup.check_and_mark("BTCUSDT", Side::Sell));
        dedup.remove_opposite("BTCUSDT", Side::Buy);
        // SELL lock was removed as "opposite of BUY"
        assert!(dedup.check_and_mark("BTCUSDT", Side::Sell));
    }

    #[test]
    fn ttl_expiry_allows_repeat() {
        let dedup = Deduplicator::new(1);
        assert!(dedup.check_and_mark("SOLUSDT", Side::Buy));
        std::thread::sleep(Duration::from_millis(1_050));
        assert!(dedup.check_and_mark("SOLUSDT", Side::Buy));
    }

    #[test]
    fn sweep_drops_expired_entries() {
        let dedup = Deduplicator::new(1);
        assert!(dedup.check_and_mark("ADAUSDT", Side::Buy));
        assert_eq!(dedup.len(), 1);
        std::thread::sleep(Duration::from_millis(1_050));
        dedup.sweep();
        assert_eq!(dedup.len(), 0);
    }

    #[test]
    fn key_is_deterministic_and_discriminating() {
        assert_eq!(key("BTCUSDT", Side::Buy), key("BTCUSDT", Side::Buy));
        assert_ne!(key("BTCUSDT", Side::Buy), key("BTCUSDT", Side::Sell));
        assert_ne!(key("BTCUSDT", Side::Buy), key("ETHUSDT", Side::Buy));
    }
}
