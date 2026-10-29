//! Percentile summaries for measured durations, as the §6 budgets are stated.

use serde::Serialize;
use std::time::Duration;

/// A summary of samples, in microseconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Summary {
    pub samples: usize,
    pub mean_us: f64,
    pub p50_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
}

impl Summary {
    /// Summarizes `samples`, or returns `None` if there are none.
    pub fn of(samples: &[Duration]) -> Option<Summary> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e6).collect();
        sorted.sort_by(f64::total_cmp);
        let percentile = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
        Some(Summary {
            samples: sorted.len(),
            mean_us: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p50_us: percentile(0.50),
            p99_us: percentile(0.99),
            max_us: sorted[sorted.len() - 1],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_one_to_a_hundred() {
        let samples: Vec<Duration> = (1..=100).map(Duration::from_micros).collect();
        let summary = Summary::of(&samples).unwrap();
        assert_eq!(summary.p50_us.round(), 51.0);
        assert_eq!(summary.p99_us.round(), 99.0);
        assert_eq!(summary.max_us.round(), 100.0);
        assert!(Summary::of(&[]).is_none());
    }
}
