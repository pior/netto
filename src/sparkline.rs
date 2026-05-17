use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::ping::{PingResult, Sample};

pub const BUCKET_COUNT: usize = 10;
pub const BUCKET_SIZE: Duration = Duration::from_secs(30);

/// Default latency thresholds. `GOOD_MS` — at or below: fully "green".
/// `BAD_MS` — at or above: latency badness saturates at 1.
const GOOD_MS_DEFAULT: f64 = 30.0;
/// `BAD_MS` is scaled with `GOOD_MS` so the green→yellow→red shape stays
/// proportional when the user moves the tolerance slider.
const BAD_TO_GOOD_RATIO: f64 = 250.0 / 30.0;

/// Runtime "good" latency threshold (bit-packed f64). 0 == unset → default.
static GOOD_MS_BITS: AtomicU64 = AtomicU64::new(0);

/// Set the "good" latency threshold in milliseconds. The "bad" threshold
/// scales proportionally.
pub fn set_tolerance_ms(good_ms: f64) {
    let v = if good_ms.is_finite() && good_ms > 0.0 {
        good_ms
    } else {
        GOOD_MS_DEFAULT
    };
    GOOD_MS_BITS.store(v.to_bits(), Ordering::Relaxed);
}

fn thresholds() -> (f64, f64) {
    let bits = GOOD_MS_BITS.load(Ordering::Relaxed);
    let good = if bits == 0 {
        GOOD_MS_DEFAULT
    } else {
        f64::from_bits(bits)
    };
    (good, good * BAD_TO_GOOD_RATIO)
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct BucketInfo {
    pub samples: u32,
    pub failed: u32,
    /// Worst (max) successful latency observed in this bucket.
    pub max_ok_ms: Option<f64>,
}

impl BucketInfo {
    pub const EMPTY: Self = BucketInfo {
        samples: 0,
        failed: 0,
        max_ok_ms: None,
    };

    pub fn is_empty(&self) -> bool {
        self.samples == 0
    }

    pub fn loss_rate(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.failed as f64 / self.samples as f64
        }
    }
}

/// Bucket samples into `BUCKET_COUNT` slots. Slot 0 is the oldest, the
/// last is the most recent (left-to-right reads as time advancing).
pub fn bucketize(samples: &[Sample], now: Instant) -> Vec<BucketInfo> {
    let mut buckets = vec![BucketInfo::EMPTY; BUCKET_COUNT];
    for s in samples {
        let age = now.saturating_duration_since(s.at);
        let idx_from_now = (age.as_secs_f64() / BUCKET_SIZE.as_secs_f64()) as usize;
        if idx_from_now >= BUCKET_COUNT {
            continue;
        }
        let idx = BUCKET_COUNT - 1 - idx_from_now;
        let b = &mut buckets[idx];
        match &s.result {
            PingResult::Ok(ms) => {
                b.samples += 1;
                b.max_ok_ms = Some(match b.max_ok_ms {
                    Some(p) => p.max(*ms),
                    None => *ms,
                });
            }
            PingResult::Timeout | PingResult::Error(_) => {
                b.samples += 1;
                b.failed += 1;
            }
            PingResult::Pending => {}
        }
    }
    buckets
}

/// Maps a bucket to a 0.0..1.0 "badness" score, or `None` for an empty
/// bucket. 0 = perfect (low latency, no loss); 1 = worst (high latency or
/// total loss). Score is the worse of two independent components:
/// latency (mapped via smoothstep over GOOD_MS..BAD_MS) and packet loss
/// rate (linear 0..1).
pub fn badness(b: &BucketInfo) -> Option<f64> {
    if b.samples == 0 {
        return None;
    }
    let (good, bad) = thresholds();
    let latency = match b.max_ok_ms {
        Some(ms) => smoothstep(good, bad, ms),
        None => 1.0,
    };
    let loss = b.loss_rate();
    Some(latency.max(loss))
}

fn smoothstep(low: f64, high: f64, x: f64) -> f64 {
    let t = ((x - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Aggregate multiple bucketized rows with best-of-N per slot, scored by
/// `badness`. Empty buckets are ignored; if every row is empty at slot i,
/// the result is Empty.
pub fn best_of(rows: &[Vec<BucketInfo>]) -> Vec<BucketInfo> {
    let len = rows.iter().map(Vec::len).min().unwrap_or(0);
    (0..len)
        .map(|i| {
            rows.iter()
                .map(|r| r[i])
                .filter(|b| !b.is_empty())
                .min_by(|a, b| {
                    let sa = badness(a).unwrap_or(0.0);
                    let sb = badness(b).unwrap_or(0.0);
                    sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(BucketInfo::EMPTY)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(secs_ago: u64, result: PingResult) -> Sample {
        Sample {
            at: Instant::now() - Duration::from_secs(secs_ago),
            result,
        }
    }

    #[test]
    fn badness_low_when_fast_and_clean() {
        let b = BucketInfo {
            samples: 10,
            failed: 0,
            max_ok_ms: Some(10.0),
        };
        assert_eq!(badness(&b), Some(0.0));
    }

    #[test]
    fn badness_high_for_total_loss() {
        let b = BucketInfo {
            samples: 5,
            failed: 5,
            max_ok_ms: None,
        };
        assert_eq!(badness(&b), Some(1.0));
    }

    #[test]
    fn badness_picks_worse_of_latency_and_loss() {
        // Low latency but 50% loss: badness is at least 0.5.
        let b = BucketInfo {
            samples: 10,
            failed: 5,
            max_ok_ms: Some(10.0),
        };
        assert!(badness(&b).unwrap() >= 0.5);
    }

    #[test]
    fn bucketize_places_recent_on_right() {
        let now = Instant::now();
        let samples = vec![
            Sample { at: now, result: PingResult::Ok(5.0) },
            Sample {
                at: now - BUCKET_SIZE * (BUCKET_COUNT as u32 - 1),
                result: PingResult::Ok(5.0),
            },
        ];
        let buckets = bucketize(&samples, now);
        assert_eq!(buckets.len(), BUCKET_COUNT);
        assert_eq!(buckets[BUCKET_COUNT - 1].max_ok_ms, Some(5.0));
        assert_eq!(buckets[0].max_ok_ms, Some(5.0));
    }

    #[test]
    fn bucketize_counts_loss() {
        let now = Instant::now();
        let samples = vec![
            Sample { at: now, result: PingResult::Ok(5.0) },
            Sample { at: now, result: PingResult::Ok(40.0) },
            Sample { at: now, result: PingResult::Timeout },
        ];
        let last = bucketize(&samples, now)[BUCKET_COUNT - 1];
        assert_eq!(last.samples, 3);
        assert_eq!(last.failed, 1);
        assert_eq!(last.max_ok_ms, Some(40.0));
        assert!((last.loss_rate() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn best_of_picks_lowest_badness() {
        let fast_clean = BucketInfo {
            samples: 10,
            failed: 0,
            max_ok_ms: Some(10.0),
        };
        let lossy = BucketInfo {
            samples: 10,
            failed: 5,
            max_ok_ms: Some(10.0),
        };
        let empty = BucketInfo::EMPTY;
        let agg = best_of(&[
            vec![lossy; 1],
            vec![fast_clean; 1],
            vec![empty; 1],
        ]);
        assert_eq!(agg[0], fast_clean);
    }

    #[test]
    fn drops_old_samples() {
        let now = Instant::now();
        let beyond = BUCKET_SIZE.as_secs() * BUCKET_COUNT as u64 + 10;
        let too_old = sample(beyond, PingResult::Ok(5.0));
        let buckets = bucketize(&[too_old], now);
        assert!(buckets.iter().all(|b| b.is_empty()));
    }
}
