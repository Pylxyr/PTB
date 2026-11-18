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

/// A count as `f64`. Counts here are sample and chunk counts, far below `u32::MAX`; a larger one is clamped,
/// which keeps the conversion exact for everything that can really happen.
#[must_use]
pub fn count_f64(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// Nearest-rank percentile of an ascending slice: the smallest value with at least `percent` percent of the
/// samples at or below it (more than 100 counts as 100). `None` for an empty slice.
#[must_use]
pub fn percentile(sorted: &[f64], percent: u32) -> Option<f64> {
    let last = sorted.len().checked_sub(1)?;
    let percent = usize::try_from(percent.min(100)).unwrap_or(100);
    let rank = (percent * sorted.len()).div_ceil(100);
    Some(sorted[rank.saturating_sub(1).min(last)])
}

/// Summarise samples. NaN samples are ignored. `None` when nothing usable is left.
#[must_use]
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let mut sorted: Vec<f64> = samples.iter().copied().filter(|s| !s.is_nan()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let sum: f64 = sorted.iter().sum();
    let count = count_f64(sorted.len());
    Some(Summary {
        count: sorted.len(),
        min: *sorted.first()?,
        mean: sum / count,
        p50: percentile(&sorted, 50)?,
        p95: percentile(&sorted, 95)?,
        p99: percentile(&sorted, 99)?,
        max: *sorted.last()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 50), Some(50.0));
        assert_eq!(percentile(&v, 95), Some(95.0));
        assert_eq!(percentile(&v, 99), Some(99.0));
        assert_eq!(percentile(&v, 100), Some(100.0));
        assert_eq!(percentile(&v, 0), Some(1.0));
    }

    #[test]
    fn percentile_of_nothing_is_none() {
        assert_eq!(percentile(&[], 50), None);
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
    fn counts_convert_exactly_and_clamp() {
        assert!((count_f64(496) - 496.0).abs() < f64::EPSILON);
        assert!((count_f64(usize::MAX) - f64::from(u32::MAX)).abs() < f64::EPSILON);
    }

    #[test]
    fn percentiles_above_100_count_as_100() {
        let v = [1.0, 2.0, 3.0];
        assert_eq!(percentile(&v, 0), Some(1.0));
        assert_eq!(percentile(&v, 500), Some(3.0));
    }
}
