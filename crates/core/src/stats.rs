//! Latency statistics: a sorted summary with nearest-rank percentiles.

/// Summary of a set of samples (milliseconds in this project, but the maths is unit-free).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub min: f64,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

/// Nearest-rank percentile of an ascending slice: the smallest value with at least `p` percent of the
/// samples at or below it. `None` for an empty slice.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let p = p.clamp(0.0, 100.0);
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// Summarise samples. NaN samples are ignored. `None` when nothing usable is left.
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|s| !s.is_nan()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(|a, b| a.total_cmp(b));
    let sum: f64 = sorted.iter().sum();
    Some(Summary {
        count: sorted.len(),
        min: sorted[0],
        mean: sum / sorted.len() as f64,
        p50: percentile(&sorted, 50.0)?,
        p95: percentile(&sorted, 95.0)?,
        p99: percentile(&sorted, 99.0)?,
        max: sorted[sorted.len() - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), Some(50.0));
        assert_eq!(percentile(&v, 95.0), Some(95.0));
        assert_eq!(percentile(&v, 99.0), Some(99.0));
        assert_eq!(percentile(&v, 100.0), Some(100.0));
        assert_eq!(percentile(&v, 0.0), Some(1.0));
    }

    #[test]
    fn percentile_of_nothing_is_none() {
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(summarize(&[]), None);
        assert_eq!(summarize(&[f64::NAN]), None);
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let s = summarize(&[7.5]).unwrap();
        assert_eq!(
            (s.count, s.min, s.p50, s.p95, s.p99, s.max),
            (1, 7.5, 7.5, 7.5, 7.5, 7.5)
        );
    }

    #[test]
    fn summary_sorts_and_ignores_nan() {
        let s = summarize(&[30.0, f64::NAN, 10.0, 20.0, 40.0]).unwrap();
        assert_eq!(s.count, 4);
        assert_eq!((s.min, s.max), (10.0, 40.0));
        assert!((s.mean - 25.0).abs() < 1e-9);
        assert_eq!(s.p50, 20.0);
    }

    #[test]
    fn out_of_range_percentiles_are_clamped() {
        let v = [1.0, 2.0, 3.0];
        assert_eq!(percentile(&v, -5.0), Some(1.0));
        assert_eq!(percentile(&v, 500.0), Some(3.0));
    }
}
