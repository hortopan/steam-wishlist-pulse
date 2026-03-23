use std::collections::HashMap;

use crate::db::{Database, SnapshotDelta};
use crate::steam::WishlistReport;

/// Enum representing a rate metric field, replacing stringly-typed field selection.
#[derive(Clone, Copy)]
enum RateField {
    Adds,
    Deletes,
    Purchases,
    Gifts,
}

impl RateField {
    fn name(self) -> &'static str {
        match self {
            RateField::Adds => "adds",
            RateField::Deletes => "deletes",
            RateField::Purchases => "purchases",
            RateField::Gifts => "gifts",
        }
    }

    fn extract(self, delta: &SnapshotDelta) -> f64 {
        match self {
            RateField::Adds => delta.adds_rate,
            RateField::Deletes => delta.deletes_rate,
            RateField::Purchases => delta.purchases_rate,
            RateField::Gifts => delta.gifts_rate,
        }
    }
}

/// Per-metric anomaly detail.
pub struct MetricAnomaly {
    pub name: &'static str,
    /// Raw delta between current and previous snapshot (for display).
    pub current_delta: i64,
    /// Normalized rate (per day) of the current delta.
    pub current_rate: f64,
    pub mean: f64,
    pub std_dev: f64,
    pub threshold_low: f64,
    pub threshold_high: f64,
    pub is_anomalous: bool,
}

/// Country-level anomaly detail.
pub struct CountryAnomaly {
    pub country_code: String,
    pub metric: &'static str,
    /// Raw delta between current and previous snapshot (for display).
    pub current_delta: i64,
    pub current_rate: f64,
    pub mean: f64,
    pub std_dev: f64,
}

/// Result of anomaly detection for a single snapshot change.
pub struct AnomalyResult {
    /// True if at least one metric is anomalous.
    pub is_anomalous: bool,
    /// True if there was insufficient data to compute a baseline.
    pub insufficient_data: bool,
    /// True if detection failed due to a transient error (e.g. DB).
    pub error: bool,
    /// Per-metric anomaly details.
    pub metrics: Vec<MetricAnomaly>,
    /// Country-level anomalies (only populated for flagged countries).
    pub country_anomalies: Vec<CountryAnomaly>,
}

/// Configuration for anomaly detection.
pub struct AnomalyConfig {
    pub lookback_days: u32,
    /// Sensitivity multiplier for upward deviations (spikes).
    pub sensitivity_up: f64,
    /// Sensitivity multiplier for downward deviations (drops).
    pub sensitivity_down: f64,
    pub min_absolute: i64,
    /// Floor for MAD as a fraction of the median (e.g. 0.05 = 5%).
    /// Prevents false positives when the baseline is very stable.
    pub mad_floor_pct: f64,
}

impl Default for AnomalyConfig {
    fn default() -> Self {
        Self {
            lookback_days: 14,
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 5,
            mad_floor_pct: 0.05,
        }
    }
}

/// Detect anomalies by comparing the current delta rate against historical distribution.
///
/// All deltas are normalized to rates (per day) using the actual time elapsed between
/// snapshots. This ensures consistent comparison regardless of polling frequency.
///
/// Uses a robust modified z-score approach based on **median + MAD** (Median Absolute
/// Deviation) instead of mean + std_dev.  A metric is anomalous when its modified
/// z-score exceeds `sensitivity_up` (for increases) or `sensitivity_down` (for
/// decreases) AND the raw delta exceeds `min_absolute`.
pub async fn detect_anomalies(
    db: &Database,
    app_id: u32,
    current: &WishlistReport,
    previous: &WishlistReport,
    config: &AnomalyConfig,
) -> AnomalyResult {
    // Use previous snapshot's timestamp as cutoff so the current delta is excluded from baseline
    let exclude_after = previous.fetched_at.as_deref();
    let historical_deltas = match db.get_recent_deltas(app_id, config.lookback_days, exclude_after).await {
        Ok(deltas) => deltas,
        Err(e) => {
            tracing::warn!("Failed to fetch historical deltas for anomaly detection (app {app_id}): {e}");
            return AnomalyResult {
                is_anomalous: false,
                insufficient_data: false,
                error: true,
                metrics: Vec::new(),
                country_anomalies: Vec::new(),
            };
        }
    };

    // Need at least 3 data points for a meaningful baseline
    if historical_deltas.len() < 3 {
        return AnomalyResult {
            is_anomalous: false,
            insufficient_data: true,
            error: false,
            metrics: Vec::new(),
            country_anomalies: Vec::new(),
        };
    }

    // Compute the time-normalized rate for the current change
    let days_elapsed = match (&previous.fetched_at, &current.fetched_at) {
        (Some(prev_ts), Some(curr_ts)) => crate::db::elapsed_days(prev_ts, curr_ts),
        _ => 1.0, // fallback: assume ~1 day if timestamps unavailable
    };
    let days_elapsed = if days_elapsed <= 0.0 { 1.0 } else { days_elapsed };

    let current_deltas = [
        (RateField::Adds, safe_delta(current.adds, previous.adds)),
        (RateField::Deletes, safe_delta(current.deletes, previous.deletes)),
        (RateField::Purchases, safe_delta(current.purchases, previous.purchases)),
        (RateField::Gifts, safe_delta(current.gifts, previous.gifts)),
    ];

    let mut metrics = Vec::with_capacity(4);
    let mut any_anomalous = false;

    for (rate_field, raw_delta) in &current_deltas {
        let current_rate = *raw_delta as f64 / days_elapsed;

        let mut historical_rates: Vec<f64> = historical_deltas
            .iter()
            .map(|d| rate_field.extract(d))
            .collect();

        let median = f64_median(&mut historical_rates);
        let mad = f64_mad(&mut historical_rates, median);
        let effective_mad = apply_mad_floor(mad, median, config.mad_floor_pct);
        let (threshold_low, threshold_high) = thresholds_directional(median, effective_mad, config);

        let is_anomalous = is_rate_anomalous(current_rate, *raw_delta, median, effective_mad, config);

        if is_anomalous {
            any_anomalous = true;
        }

        metrics.push(MetricAnomaly {
            name: rate_field.name(),
            current_delta: *raw_delta,
            current_rate,
            mean: median,
            std_dev: effective_mad,
            threshold_low,
            threshold_high,
            is_anomalous,
        });
    }

    // Country-level anomaly detection
    let country_anomalies = detect_country_anomalies(db, app_id, current, previous, days_elapsed, config).await;
    if !country_anomalies.is_empty() {
        any_anomalous = true;
    }

    AnomalyResult {
        is_anomalous: any_anomalous,
        insufficient_data: false,
        error: false,
        metrics,
        country_anomalies,
    }
}

/// Detect country-level anomalies for adds and deletes.
async fn detect_country_anomalies(
    db: &Database,
    app_id: u32,
    current: &WishlistReport,
    previous: &WishlistReport,
    days_elapsed: f64,
    config: &AnomalyConfig,
) -> Vec<CountryAnomaly> {
    let exclude_after = previous.fetched_at.as_deref();
    let historical = match db.get_recent_country_deltas(app_id, config.lookback_days, exclude_after).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("Failed to fetch country deltas for anomaly detection (app {app_id}): {e}");
            return Vec::new();
        }
    };

    let current_countries: HashMap<&str, &crate::steam::CountryReport> = current
        .countries
        .iter()
        .map(|c| (c.country_code.as_str(), c))
        .collect();

    let previous_countries: HashMap<&str, &crate::steam::CountryReport> = previous
        .countries
        .iter()
        .map(|c| (c.country_code.as_str(), c))
        .collect();

    let mut anomalies = Vec::new();

    for (country_code, country_deltas) in &historical {
        if country_deltas.len() < 3 {
            continue;
        }

        let curr = current_countries.get(country_code.as_str());
        let prev = previous_countries.get(country_code.as_str());

        // Check adds
        let current_adds = curr.map(|c| c.adds).unwrap_or(0);
        let previous_adds = prev.map(|c| c.adds).unwrap_or(0);
        let adds_raw = safe_delta(current_adds, previous_adds);
        let adds_rate = adds_raw as f64 / days_elapsed;

        let mut historical_adds: Vec<f64> = country_deltas.iter().map(|d| d.adds_rate).collect();
        let adds_median = f64_median(&mut historical_adds);
        let adds_mad = apply_mad_floor(f64_mad(&mut historical_adds, adds_median), adds_median, config.mad_floor_pct);

        if is_rate_anomalous(adds_rate, adds_raw, adds_median, adds_mad, config) {
            anomalies.push(CountryAnomaly {
                country_code: country_code.clone(),
                metric: "adds",
                current_delta: adds_raw,
                current_rate: adds_rate,
                mean: adds_median,
                std_dev: adds_mad,
            });
        }

        // Check deletes
        let current_deletes = curr.map(|c| c.deletes).unwrap_or(0);
        let previous_deletes = prev.map(|c| c.deletes).unwrap_or(0);
        let deletes_raw = safe_delta(current_deletes, previous_deletes);
        let deletes_rate = deletes_raw as f64 / days_elapsed;

        let mut historical_deletes: Vec<f64> = country_deltas.iter().map(|d| d.deletes_rate).collect();
        let deletes_median = f64_median(&mut historical_deletes);
        let deletes_mad = apply_mad_floor(f64_mad(&mut historical_deletes, deletes_median), deletes_median, config.mad_floor_pct);

        if is_rate_anomalous(deletes_rate, deletes_raw, deletes_median, deletes_mad, config) {
            anomalies.push(CountryAnomaly {
                country_code: country_code.clone(),
                metric: "deletes",
                current_delta: deletes_raw,
                current_rate: deletes_rate,
                mean: deletes_median,
                std_dev: deletes_mad,
            });
        }
    }

    // Sort by absolute rate descending for most impactful first
    anomalies.sort_by(|a, b| b.current_rate.abs().partial_cmp(&a.current_rate.abs()).unwrap_or(std::cmp::Ordering::Equal));

    anomalies
}

/// Compute delta between two i64 values.
fn safe_delta(current: i64, previous: i64) -> i64 {
    current - previous
}

/// Determine if a rate is anomalous using a modified z-score (median + MAD).
///
/// A rate is anomalous when:
/// - The raw delta is non-zero, AND
/// - The raw delta's absolute value meets `min_absolute`, AND
/// - The modified z-score exceeds `sensitivity_up` (for rate > median) or
///   `sensitivity_down` (for rate < median)
///
/// When MAD is 0 (perfectly stable history), any rate change meeting
/// `min_absolute` is considered anomalous.
fn is_rate_anomalous(rate: f64, raw_delta: i64, median: f64, mad: f64, config: &AnomalyConfig) -> bool {
    if raw_delta == 0 {
        return false;
    }
    // When the baseline median is near zero, any non-zero activity is statistically
    // significant — skip the min_absolute gate so early signals aren't suppressed.
    if median.abs() > f64::EPSILON && raw_delta.abs() < config.min_absolute {
        return false;
    }
    if mad == 0.0 {
        return (rate - median).abs() > f64::EPSILON;
    }
    let deviation = rate - median;
    let z = deviation.abs() / mad;
    if deviation >= 0.0 {
        z > config.sensitivity_up
    } else {
        z > config.sensitivity_down
    }
}

/// Compute the median of a f64 slice (sorts in place).
fn f64_median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n % 2 == 0 {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    }
}

/// Compute MAD (Median Absolute Deviation) scaled to be consistent with std_dev.
///
/// MAD = median(|xi - median(x)|) × 1.4826
///
/// The constant 1.4826 makes MAD a consistent estimator of standard deviation
/// for normally distributed data, while remaining robust to outliers.
fn f64_mad(values: &mut [f64], median: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mut abs_devs: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
    let raw_mad = f64_median(&mut abs_devs);
    raw_mad * 1.4826
}

/// Apply a floor to MAD to prevent false positives on very stable baselines.
///
/// The floor is `mad_floor_pct × |median|`, ensuring that even when the baseline
/// has near-zero variance, we don't flag trivial fluctuations.
fn apply_mad_floor(mad: f64, median: f64, floor_pct: f64) -> f64 {
    let floor = median.abs() * floor_pct;
    mad.max(floor)
}

/// Compute directional anomaly thresholds: median - sensitivity_down × MAD .. median + sensitivity_up × MAD.
fn thresholds_directional(median: f64, mad: f64, config: &AnomalyConfig) -> (f64, f64) {
    (
        median - config.sensitivity_down * mad,
        median + config.sensitivity_up * mad,
    )
}

// Public re-exports for web.rs chart anomaly logic (avoids duplicating math).
pub fn f64_median_pub(values: &mut [f64]) -> f64 { f64_median(values) }
pub fn f64_mad_pub(values: &mut [f64], median: f64) -> f64 { f64_mad(values, median) }
pub fn apply_mad_floor_pub(mad: f64, median: f64, floor_pct: f64) -> f64 { apply_mad_floor(mad, median, floor_pct) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f64_median() {
        assert_eq!(f64_median(&mut []), 0.0);
        assert_eq!(f64_median(&mut [5.0]), 5.0);
        assert_eq!(f64_median(&mut [3.0, 1.0, 2.0]), 2.0);
        // Even count: average of two middle values
        assert_eq!(f64_median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(f64_median(&mut [0.0, 0.0, 0.0]), 0.0);
        // Negative values
        assert!(f64_median(&mut [-3.0, 5.0, -7.0]) - (-3.0) < 1e-10);
    }

    #[test]
    fn test_f64_mad() {
        // Constant values → MAD = 0
        assert_eq!(f64_mad(&mut [10.0, 10.0, 10.0], 10.0), 0.0);
        // Single value → 0
        assert_eq!(f64_mad(&mut [5.0], 5.0), 0.0);
        // Known case: [2, 4, 4, 4, 5, 5, 7, 9], median=4.5
        // Absolute deviations: [2.5, 0.5, 0.5, 0.5, 0.5, 0.5, 2.5, 4.5]
        // Sorted: [0.5, 0.5, 0.5, 0.5, 0.5, 2.5, 2.5, 4.5]
        // Median of deviations = (0.5 + 2.5) / 2 = 1.5 (even count, average of 4th and 5th)
        // Wait: sorted [0.5, 0.5, 0.5, 0.5, 0.5, 2.5, 2.5, 4.5], n=8, mid = (vals[3]+vals[4])/2 = (0.5+0.5)/2 = 0.5
        // MAD = 0.5 * 1.4826 = 0.7413
        let mut vals = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let median = f64_median(&mut vals);
        let mad = f64_mad(&mut vals, median);
        assert!((mad - 0.5 * 1.4826).abs() < 1e-10);
    }

    #[test]
    fn test_apply_mad_floor() {
        // MAD below floor → use floor
        assert!((apply_mad_floor(0.0, 100.0, 0.05) - 5.0).abs() < 1e-10);
        // MAD above floor → use MAD
        assert!((apply_mad_floor(10.0, 100.0, 0.05) - 10.0).abs() < 1e-10);
        // Median of 0 → floor is 0, MAD wins
        assert!((apply_mad_floor(1.0, 0.0, 0.05) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_thresholds_directional() {
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 3.0,
            ..Default::default()
        };
        let (lo, hi) = thresholds_directional(10.0, 5.0, &config);
        // hi = 10 + 2*5 = 20
        assert!((hi - 20.0).abs() < 1e-10);
        // lo = 10 - 3*5 = -5
        assert!((lo - (-5.0)).abs() < 1e-10);
    }

    #[test]
    fn test_is_rate_anomalous_zero_delta() {
        let config = AnomalyConfig::default();
        assert!(!is_rate_anomalous(10.0, 0, 10.0, 5.0, &config));
    }

    #[test]
    fn test_is_rate_anomalous_below_min_absolute() {
        let config = AnomalyConfig { min_absolute: 10, ..Default::default() };
        // Raw delta of 5 is below min_absolute of 10 (with non-zero median, gate applies)
        assert!(!is_rate_anomalous(50.0, 5, 50.0, 5.0, &config));
    }

    #[test]
    fn test_is_rate_anomalous_near_zero_baseline_bypasses_min_absolute() {
        let config = AnomalyConfig { min_absolute: 5, ..Default::default() };
        // When median is 0, min_absolute gate is skipped — small deltas are flagged
        assert!(is_rate_anomalous(2.0, 2, 0.0, 0.0, &config));
        assert!(is_rate_anomalous(1.0, 1, 0.0, 0.0, &config));
        // Zero delta is still never anomalous
        assert!(!is_rate_anomalous(0.0, 0, 0.0, 0.0, &config));
    }

    #[test]
    fn test_is_rate_anomalous_constant_history() {
        let config = AnomalyConfig { min_absolute: 1, ..Default::default() };
        // History was constant at 0 rate, MAD=0, any non-zero rate is anomalous
        assert!(is_rate_anomalous(10.0, 10, 0.0, 0.0, &config));
        // Rate matches median → not anomalous
        assert!(!is_rate_anomalous(10.0, 10, 10.0, 0.0, &config));
    }

    #[test]
    fn test_is_rate_anomalous_modified_z_score() {
        // median=10, MAD=5, sensitivity_up=2.0, sensitivity_down=2.0
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 1,
            ..Default::default()
        };
        // rate=15 → z=1.0, not anomalous
        assert!(!is_rate_anomalous(15.0, 15, 10.0, 5.0, &config));
        // rate=25 → z=3.0, anomalous (above)
        assert!(is_rate_anomalous(25.0, 25, 10.0, 5.0, &config));
        // rate=-5 → z=3.0, anomalous (below)
        assert!(is_rate_anomalous(-5.0, -5, 10.0, 5.0, &config));
        // rate=10 → z=0.0, not anomalous (exactly at median)
        assert!(!is_rate_anomalous(10.0, 10, 10.0, 5.0, &config));
    }

    #[test]
    fn test_directional_sensitivity() {
        // Different sensitivity for up vs down
        let config = AnomalyConfig {
            sensitivity_up: 3.0,  // lenient for spikes
            sensitivity_down: 1.5, // strict for drops
            min_absolute: 1,
            ..Default::default()
        };
        // median=10, MAD=5
        // rate=25 → z=3.0 upward, sensitivity_up=3.0 → NOT anomalous (at boundary)
        assert!(!is_rate_anomalous(25.0, 25, 10.0, 5.0, &config));
        // rate=26 → z=3.2 upward → anomalous
        assert!(is_rate_anomalous(26.0, 26, 10.0, 5.0, &config));
        // rate=2 → z=1.6 downward, sensitivity_down=1.5 → anomalous
        assert!(is_rate_anomalous(2.0, -8, 10.0, 5.0, &config));
        // rate=5 → z=1.0 downward → not anomalous
        assert!(!is_rate_anomalous(5.0, -5, 10.0, 5.0, &config));
    }

    #[test]
    fn test_directional_detection() {
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 5,
            ..Default::default()
        };
        // median=50/day, MAD=3/day → thresholds [44, 56]
        // rate=-10/day → z = |-10 - 50| / 3 = 20.0 → anomalous
        assert!(is_rate_anomalous(-10.0, -10, 50.0, 3.0, &config));
        // rate=48/day → z = |48 - 50| / 3 = 0.67 → not anomalous
        assert!(!is_rate_anomalous(48.0, 48, 50.0, 3.0, &config));
    }

    #[test]
    fn test_rate_normalization_concept() {
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 1,
            ..Default::default()
        };
        // Historical baseline: ~50/day with MAD=5
        // +100 over 2 days = 50/day rate → normal
        assert!(!is_rate_anomalous(50.0, 100, 50.0, 5.0, &config));
        // +100 over 0.5 days = 200/day rate → anomalous
        assert!(is_rate_anomalous(200.0, 100, 50.0, 5.0, &config));
    }

    #[test]
    fn test_safe_delta() {
        assert_eq!(safe_delta(100, 50), 50);
        assert_eq!(safe_delta(50, 100), -50);
        assert_eq!(safe_delta(0, 0), 0);
    }

    #[test]
    fn test_mad_floor_prevents_false_positive() {
        // Stable baseline at median=100, MAD=0 → floor kicks in (100 * 0.05 = 5.0)
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 1,
            mad_floor_pct: 0.05,
            ..Default::default()
        };
        // rate=103, median=100, effective_mad=5.0 → z=0.6 → not anomalous
        let effective_mad = apply_mad_floor(0.0, 100.0, config.mad_floor_pct);
        assert!(!is_rate_anomalous(103.0, 3, 100.0, effective_mad, &config));
        // rate=115 → z=3.0 → anomalous
        assert!(is_rate_anomalous(115.0, 15, 100.0, effective_mad, &config));
    }
}
