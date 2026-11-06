//! Lock-free counters for the hot path. They cost one relaxed atomic add and
//! make it possible to see *why* signals were dropped without grepping logs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Default)]
pub struct Metrics {
    pub signals: AtomicU64,
    pub invalid_signals: AtomicU64,
    pub duplicates: AtomicU64,
    pub no_ticker: AtomicU64,
    pub no_filters: AtomicU64,
    pub no_balance: AtomicU64,
    pub below_min: AtomicU64,
    pub orders_ok: AtomicU64,
    pub orders_failed: AtomicU64,
    pub dry_runs: AtomicU64,
    pub tpsl_ok: AtomicU64,
    pub tpsl_partial: AtomicU64,
    pub tpsl_failed: AtomicU64,
    pub fast_fills: AtomicU64,
    pub polled_fills: AtomicU64,
    latency_samples: AtomicU64,
    latency_sum_us: AtomicU64,
    latency_max_us: AtomicU64,
}

/// Point-in-time copy used for logging.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    pub signals: u64,
    pub invalid_signals: u64,
    pub duplicates: u64,
    pub no_ticker: u64,
    pub no_filters: u64,
    pub no_balance: u64,
    pub below_min: u64,
    pub orders_ok: u64,
    pub orders_failed: u64,
    pub dry_runs: u64,
    pub tpsl_ok: u64,
    pub tpsl_partial: u64,
    pub tpsl_failed: u64,
    pub fast_fills: u64,
    pub polled_fills: u64,
    pub latency_samples: u64,
    pub latency_avg_us: u64,
    pub latency_max_us: u64,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[inline]
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Records order round-trip time (signal -> order acknowledgement).
    pub fn observe_latency(&self, micros: u64) {
        self.latency_samples.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_us.fetch_add(micros, Ordering::Relaxed);
        self.latency_max_us.fetch_max(micros, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        let sum = self.latency_sum_us.load(Ordering::Relaxed);
        MetricsSnapshot {
            signals: self.signals.load(Ordering::Relaxed),
            invalid_signals: self.invalid_signals.load(Ordering::Relaxed),
            duplicates: self.duplicates.load(Ordering::Relaxed),
            no_ticker: self.no_ticker.load(Ordering::Relaxed),
            no_filters: self.no_filters.load(Ordering::Relaxed),
            no_balance: self.no_balance.load(Ordering::Relaxed),
            below_min: self.below_min.load(Ordering::Relaxed),
            orders_ok: self.orders_ok.load(Ordering::Relaxed),
            orders_failed: self.orders_failed.load(Ordering::Relaxed),
            dry_runs: self.dry_runs.load(Ordering::Relaxed),
            tpsl_ok: self.tpsl_ok.load(Ordering::Relaxed),
            tpsl_partial: self.tpsl_partial.load(Ordering::Relaxed),
            tpsl_failed: self.tpsl_failed.load(Ordering::Relaxed),
            fast_fills: self.fast_fills.load(Ordering::Relaxed),
            polled_fills: self.polled_fills.load(Ordering::Relaxed),
            latency_samples: samples,
            latency_avg_us: if samples == 0 { 0 } else { sum / samples },
            latency_max_us: self.latency_max_us.load(Ordering::Relaxed),
        }
    }
}

impl std::fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "signals={} dup={} invalid={} no_ticker={} no_filters={} no_balance={} below_min={} | \
             orders ok={} fail={} dry={} rtt avg={}us max={}us (n={}) | \
             tpsl ok={} partial={} fail={} | fill fast={} polled={}",
            self.signals,
            self.duplicates,
            self.invalid_signals,
            self.no_ticker,
            self.no_filters,
            self.no_balance,
            self.below_min,
            self.orders_ok,
            self.orders_failed,
            self.dry_runs,
            self.latency_avg_us,
            self.latency_max_us,
            self.latency_samples,
            self.tpsl_ok,
            self.tpsl_partial,
            self.tpsl_failed,
            self.fast_fills,
            self.polled_fills,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_aggregation() {
        let metrics = Metrics::new();
        metrics.observe_latency(100);
        metrics.observe_latency(300);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.latency_samples, 2);
        assert_eq!(snapshot.latency_avg_us, 200);
        assert_eq!(snapshot.latency_max_us, 300);
    }

    #[test]
    fn counters_are_independent() {
        let metrics = Metrics::new();
        Metrics::inc(&metrics.signals);
        Metrics::inc(&metrics.signals);
        Metrics::inc(&metrics.duplicates);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.signals, 2);
        assert_eq!(snapshot.duplicates, 1);
        assert_eq!(snapshot.orders_ok, 0);
    }
}
