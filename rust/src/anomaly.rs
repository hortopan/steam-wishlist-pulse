use std::collections::HashMap;

use chrono::Datelike;

use crate::db::{DailyMax, Database};
use crate::steam::WishlistReport;

/// Enum representing a metric field.
#[derive(Clone, Copy)]
enum MetricField {
    Adds,
    Deletes,
    Purchases,
    Gifts,
}

impl MetricField {
    fn name(self) -> &'static str {
        match self {
            MetricField::Adds => "adds",
            MetricField::Deletes => "deletes",
            MetricField::Purchases => "purchases",
            MetricField::Gifts => "gifts",
        }
    }

    /// Extract this metric's daily total from a DailyMax record.
    fn extract_daily(self, d: &DailyMax) -> i64 {
        match self {
            MetricField::Adds => d.adds,
            MetricField::Deletes => d.deletes,
            MetricField::Purchases => d.purchases,
            MetricField::Gifts => d.gifts,
        }
    }
}

/// Per-metric anomaly detail.
#[allow(dead_code)]
pub struct MetricAnomaly {
    pub name: &'static str,
    /// Today's running total for this metric.
    pub current_delta: i64,
    /// Today's running total as f64 (daily value = daily rate).
    pub current_rate: f64,
    /// Median of historical daily totals.
    pub mean: f64,
    /// Effective MAD (with floor applied) of historical daily totals.
    pub std_dev: f64,
    pub threshold_low: f64,
    pub threshold_high: f64,
    pub is_anomalous: bool,
}

/// Country-level anomaly detail.
#[allow(dead_code)]
pub struct CountryAnomaly {
    pub country_code: String,
    pub metric: &'static str,
    /// Today's running total for this country+metric.
    pub current_delta: i64,
    pub current_rate: f64,
    /// Median of historical daily totals for this country+metric.
    pub mean: f64,
    /// Effective MAD of historical daily totals.
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
            lookback_days: 28,
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 5,
            mad_floor_pct: 0.05,
        }
    }
}

/// Detect anomalies by comparing today's running totals against historical daily totals.
///
/// The baseline is built from the MAX of each metric per day over the lookback window
/// (excluding today). Since Steam values are per-date running totals, MAX per day gives
/// the true daily total.
///
/// Uses a robust modified z-score approach based on **median + MAD** (Median Absolute
/// Deviation) instead of mean + std_dev. A metric is anomalous when its deviation from
/// the median exceeds `sensitivity_up` (for increases) or `sensitivity_down` (for
/// decreases) AND the deviation exceeds `min_absolute`.
pub async fn detect_anomalies(
    db: &Database,
    app_id: u32,
    current: &WishlistReport,
    _previous: &WishlistReport,
    config: &AnomalyConfig,
) -> AnomalyResult {
    // Fetch historical daily maxes, excluding today to avoid self-comparison
    let daily_maxes = match db
        .get_daily_maxes(app_id, config.lookback_days, &current.date)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch daily maxes for anomaly detection (app {app_id}): {e}"
            );
            return AnomalyResult {
                is_anomalous: false,
                insufficient_data: false,
                error: true,
                metrics: Vec::new(),
                country_anomalies: Vec::new(),
            };
        }
    };

    // Need at least 3 historical days for a meaningful baseline
    if daily_maxes.len() < 3 {
        return AnomalyResult {
            is_anomalous: false,
            insufficient_data: true,
            error: false,
            metrics: Vec::new(),
            country_anomalies: Vec::new(),
        };
    }

    // Day-of-week filtering: if we have enough same-weekday samples (>= 3),
    // prefer those over the full set to account for weekday/weekend seasonality.
    let daily_maxes = filter_by_weekday(&daily_maxes, &current.date);

    // Today's running totals are the current snapshot values.
    // These are directly comparable to historical daily maxes.
    let fields = [
        (MetricField::Adds, current.adds),
        (MetricField::Deletes, current.deletes),
        (MetricField::Purchases, current.purchases),
        (MetricField::Gifts, current.gifts),
    ];

    let mut metrics = Vec::with_capacity(4);
    let mut any_anomalous = false;

    for (field, today_total) in &fields {
        let mut historical: Vec<f64> = daily_maxes
            .iter()
            .map(|d| field.extract_daily(*d) as f64)
            .collect();

        let median = f64_median(&mut historical);
        let mad = f64_mad(&mut historical, median);
        let effective_mad = apply_mad_floor(mad, median, config.mad_floor_pct);
        let (threshold_low, threshold_high) =
            thresholds_directional(median, effective_mad, config);

        let current_f64 = *today_total as f64;
        let is_anomalous = is_value_anomalous(current_f64, median, effective_mad, config);

        if is_anomalous {
            any_anomalous = true;
        }

        metrics.push(MetricAnomaly {
            name: field.name(),
            current_delta: *today_total,
            current_rate: current_f64,
            mean: median,
            std_dev: effective_mad,
            threshold_low,
            threshold_high,
            is_anomalous,
        });
    }

    // Country-level anomaly detection
    let country_anomalies =
        detect_country_anomalies(db, app_id, current, config).await;
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

/// Detect country-level anomalies for adds and deletes by comparing today's
/// running totals against historical daily maxes per country.
async fn detect_country_anomalies(
    db: &Database,
    app_id: u32,
    current: &WishlistReport,
    config: &AnomalyConfig,
) -> Vec<CountryAnomaly> {
    let historical = match db
        .get_daily_country_maxes(app_id, config.lookback_days, &current.date)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                "Failed to fetch country daily maxes for anomaly detection (app {app_id}): {e}"
            );
            return Vec::new();
        }
    };

    let current_countries: HashMap<&str, &crate::steam::CountryReport> = current
        .countries
        .iter()
        .map(|c| (c.country_code.as_str(), c))
        .collect();

    let mut anomalies = Vec::new();

    for (country_code, daily_maxes) in &historical {
        if daily_maxes.len() < 3 {
            continue;
        }

        let curr = current_countries.get(country_code.as_str());
        let today_adds = curr.map(|c| c.adds).unwrap_or(0);
        let today_deletes = curr.map(|c| c.deletes).unwrap_or(0);

        // Check adds
        let mut hist_adds: Vec<f64> = daily_maxes.iter().map(|d| d.adds as f64).collect();
        let adds_median = f64_median(&mut hist_adds);
        let adds_mad = apply_mad_floor(
            f64_mad(&mut hist_adds, adds_median),
            adds_median,
            config.mad_floor_pct,
        );

        // Scale min_absolute for country-level: use max(global_min, 10% of median)
        // so high-volume countries need proportionally larger deviations to trigger.
        let scaled_min = (config.min_absolute).max((adds_median.abs() * 0.1) as i64);
        if is_value_anomalous_with_min(today_adds as f64, adds_median, adds_mad, config, scaled_min)
        {
            anomalies.push(CountryAnomaly {
                country_code: country_code.clone(),
                metric: "adds",
                current_delta: today_adds,
                current_rate: today_adds as f64,
                mean: adds_median,
                std_dev: adds_mad,
            });
        }

        // Check deletes
        let mut hist_deletes: Vec<f64> =
            daily_maxes.iter().map(|d| d.deletes as f64).collect();
        let deletes_median = f64_median(&mut hist_deletes);
        let deletes_mad = apply_mad_floor(
            f64_mad(&mut hist_deletes, deletes_median),
            deletes_median,
            config.mad_floor_pct,
        );

        let scaled_min =
            (config.min_absolute).max((deletes_median.abs() * 0.1) as i64);
        if is_value_anomalous_with_min(
            today_deletes as f64,
            deletes_median,
            deletes_mad,
            config,
            scaled_min,
        ) {
            anomalies.push(CountryAnomaly {
                country_code: country_code.clone(),
                metric: "deletes",
                current_delta: today_deletes,
                current_rate: today_deletes as f64,
                mean: deletes_median,
                std_dev: deletes_mad,
            });
        }
    }

    // Sort by absolute value descending for most impactful first
    anomalies.sort_by(|a, b| {
        b.current_rate
            .abs()
            .partial_cmp(&a.current_rate.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    anomalies
}

/// Parse a YYYY-MM-DD date string to a weekday number (Mon=0 .. Sun=6).
fn parse_weekday(date_str: &str) -> Option<chrono::Weekday> {
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .ok()
        .map(|d| d.weekday())
}

/// Filter historical daily maxes to same-day-of-week as the target date.
/// Returns the filtered set if >= 3 same-weekday samples exist, otherwise
/// returns the original set (graceful fallback).
fn filter_by_weekday<'a>(daily_maxes: &'a [DailyMax], target_date: &str) -> Vec<&'a DailyMax> {
    let target_weekday = match parse_weekday(target_date) {
        Some(wd) => wd,
        None => return daily_maxes.iter().collect(), // can't parse, use all
    };

    let same_weekday: Vec<&DailyMax> = daily_maxes
        .iter()
        .filter(|d| parse_weekday(&d.date) == Some(target_weekday))
        .collect();

    if same_weekday.len() >= 3 {
        same_weekday
    } else {
        daily_maxes.iter().collect()
    }
}

/// Determine if a daily total is anomalous using a modified z-score (median + MAD).
///
/// A value is anomalous when:
/// - Its deviation from the median meets `min_absolute`, AND
/// - The modified z-score exceeds `sensitivity_up` (for value > median) or
///   `sensitivity_down` (for value < median)
///
/// When MAD is 0 (perfectly stable history), any value meeting `min_absolute`
/// deviation from the median is considered anomalous.
fn is_value_anomalous(value: f64, median: f64, mad: f64, config: &AnomalyConfig) -> bool {
    is_value_anomalous_with_min(value, median, mad, config, config.min_absolute)
}

/// Like `is_value_anomalous` but with a custom `min_absolute` threshold.
/// Used for country-level detection where the threshold scales with volume.
fn is_value_anomalous_with_min(
    value: f64,
    median: f64,
    mad: f64,
    config: &AnomalyConfig,
    min_absolute: i64,
) -> bool {
    let deviation = value - median;
    if deviation.abs() < min_absolute as f64 {
        return false;
    }
    if mad == 0.0 {
        return deviation.abs() > f64::EPSILON;
    }
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
    if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    }
}

/// Compute MAD (Median Absolute Deviation) scaled to be consistent with std_dev.
///
/// MAD = median(|xi - median(x)|) * 1.4826
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
/// The floor combines a proportional component (`mad_floor_pct * |median|`) with
/// a fixed minimum (`MIN_MAD_FLOOR`). This prevents:
/// - False positives on stable high-volume metrics (proportional floor)
/// - Over-sensitivity on low-volume metrics where the proportional floor is tiny
///   (fixed floor ensures e.g. a jump from 0 to 1 isn't flagged)
const MIN_MAD_FLOOR: f64 = 2.0;

fn apply_mad_floor(mad: f64, median: f64, floor_pct: f64) -> f64 {
    let proportional_floor = median.abs() * floor_pct;
    let floor = proportional_floor.max(MIN_MAD_FLOOR);
    mad.max(floor)
}

/// Compute directional anomaly thresholds: median - sensitivity_down * MAD .. median + sensitivity_up * MAD.
fn thresholds_directional(median: f64, mad: f64, config: &AnomalyConfig) -> (f64, f64) {
    (
        median - config.sensitivity_down * mad,
        median + config.sensitivity_up * mad,
    )
}

// Public re-exports for web.rs chart anomaly logic (avoids duplicating math).
pub fn f64_median_pub(values: &mut [f64]) -> f64 {
    f64_median(values)
}
pub fn f64_mad_pub(values: &mut [f64], median: f64) -> f64 {
    f64_mad(values, median)
}
pub fn apply_mad_floor_pub(mad: f64, median: f64, floor_pct: f64) -> f64 {
    apply_mad_floor(mad, median, floor_pct)
}

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
        // Constant values -> MAD = 0
        assert_eq!(f64_mad(&mut [10.0, 10.0, 10.0], 10.0), 0.0);
        // Single value -> 0
        assert_eq!(f64_mad(&mut [5.0], 5.0), 0.0);
        // Known case: [2, 4, 4, 4, 5, 5, 7, 9], median=4.5
        // Sorted deviations: [0.5, 0.5, 0.5, 0.5, 0.5, 2.5, 2.5, 4.5]
        // n=8, mid = (vals[3]+vals[4])/2 = (0.5+0.5)/2 = 0.5
        // MAD = 0.5 * 1.4826 = 0.7413
        let mut vals = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        let median = f64_median(&mut vals);
        let mad = f64_mad(&mut vals, median);
        assert!((mad - 0.5 * 1.4826).abs() < 1e-10);
    }

    #[test]
    fn test_apply_mad_floor() {
        // MAD below proportional floor -> use proportional floor
        assert!((apply_mad_floor(0.0, 100.0, 0.05) - 5.0).abs() < 1e-10);
        // MAD above floor -> use MAD
        assert!((apply_mad_floor(10.0, 100.0, 0.05) - 10.0).abs() < 1e-10);
        // Median of 0 -> proportional floor is 0, fixed MIN_MAD_FLOOR kicks in
        assert!((apply_mad_floor(1.0, 0.0, 0.05) - MIN_MAD_FLOOR).abs() < 1e-10);
        // MAD above fixed floor -> use MAD
        assert!((apply_mad_floor(3.0, 0.0, 0.05) - 3.0).abs() < 1e-10);
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
    fn test_is_value_anomalous_small_deviation() {
        let config = AnomalyConfig::default(); // min_absolute=5
        // Deviation of 3 from median is below min_absolute=5, not anomalous
        assert!(!is_value_anomalous(53.0, 50.0, 5.0, &config));
        // Deviation of 0 from median, not anomalous
        assert!(!is_value_anomalous(50.0, 50.0, 5.0, &config));
    }

    #[test]
    fn test_is_value_anomalous_below_min_absolute() {
        let config = AnomalyConfig {
            min_absolute: 10,
            ..Default::default()
        };
        // Deviation of 5 is below min_absolute of 10
        assert!(!is_value_anomalous(55.0, 50.0, 5.0, &config));
    }

    #[test]
    fn test_is_value_anomalous_near_zero_baseline() {
        let config = AnomalyConfig {
            min_absolute: 5,
            ..Default::default()
        };
        // Baseline was zero, small values below min_absolute are not anomalous
        assert!(!is_value_anomalous(2.0, 0.0, 0.0, &config));
        assert!(!is_value_anomalous(1.0, 0.0, 0.0, &config));
        // But values at or above min_absolute ARE anomalous (deviation = value - 0)
        assert!(is_value_anomalous(5.0, 0.0, 0.0, &config));
        assert!(is_value_anomalous(10.0, 0.0, 0.0, &config));
    }

    #[test]
    fn test_is_value_anomalous_constant_history() {
        let config = AnomalyConfig {
            min_absolute: 1,
            ..Default::default()
        };
        // History was constant at 0, MAD=0, any non-zero value with enough deviation is anomalous
        assert!(is_value_anomalous(10.0, 0.0, 0.0, &config));
        // Value matches median -> deviation=0, below min_absolute -> not anomalous
        assert!(!is_value_anomalous(10.0, 10.0, 0.0, &config));
    }

    #[test]
    fn test_is_value_anomalous_modified_z_score() {
        // median=100, MAD=10, sensitivity=2.0
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 1,
            ..Default::default()
        };
        // value=115 -> z=1.5, not anomalous
        assert!(!is_value_anomalous(115.0, 100.0, 10.0, &config));
        // value=125 -> z=2.5, anomalous (above)
        assert!(is_value_anomalous(125.0, 100.0, 10.0, &config));
        // value=75 -> z=2.5, anomalous (below)
        assert!(is_value_anomalous(75.0, 100.0, 10.0, &config));
        // value=100 -> z=0.0, not anomalous (exactly at median)
        assert!(!is_value_anomalous(100.0, 100.0, 10.0, &config));
    }

    #[test]
    fn test_directional_sensitivity() {
        // Different sensitivity for up vs down
        let config = AnomalyConfig {
            sensitivity_up: 3.0,   // lenient for spikes
            sensitivity_down: 1.5, // strict for drops
            min_absolute: 1,
            ..Default::default()
        };
        // median=100, MAD=10
        // value=130 -> z=3.0 upward, sensitivity_up=3.0 -> NOT anomalous (at boundary)
        assert!(!is_value_anomalous(130.0, 100.0, 10.0, &config));
        // value=131 -> z=3.1 upward -> anomalous
        assert!(is_value_anomalous(131.0, 100.0, 10.0, &config));
        // value=84 -> z=1.6 downward, sensitivity_down=1.5 -> anomalous
        assert!(is_value_anomalous(84.0, 100.0, 10.0, &config));
        // value=90 -> z=1.0 downward -> not anomalous
        assert!(!is_value_anomalous(90.0, 100.0, 10.0, &config));
    }

    #[test]
    fn test_drop_detection() {
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 5,
            ..Default::default()
        };
        // median=50, MAD=3 -> thresholds [44, 56]
        // value=2 -> deviation=-48, z=16.0 -> anomalous (significant drop)
        assert!(is_value_anomalous(2.0, 50.0, 3.0, &config));
        // value=48 -> deviation=-2, below min_absolute=5 -> not anomalous
        assert!(!is_value_anomalous(48.0, 50.0, 3.0, &config));
    }

    #[test]
    fn test_daily_total_comparison() {
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 1,
            ..Default::default()
        };
        // Historical daily totals: median=50, MAD=5
        // Today's running total of 50 -> normal
        assert!(!is_value_anomalous(50.0, 50.0, 5.0, &config));
        // Today's running total of 200 -> anomalous spike
        assert!(is_value_anomalous(200.0, 50.0, 5.0, &config));
        // Today's running total of 5 -> anomalous drop
        assert!(is_value_anomalous(5.0, 50.0, 5.0, &config));
    }

    #[test]
    fn test_mad_floor_prevents_false_positive() {
        // Stable baseline at median=100, MAD=0 -> floor kicks in (100 * 0.05 = 5.0)
        let config = AnomalyConfig {
            sensitivity_up: 2.0,
            sensitivity_down: 2.0,
            min_absolute: 1,
            mad_floor_pct: 0.05,
            ..Default::default()
        };
        // value=103, median=100, effective_mad=5.0 -> deviation=3, z=0.6 -> not anomalous
        let effective_mad = apply_mad_floor(0.0, 100.0, config.mad_floor_pct);
        assert!(!is_value_anomalous(103.0, 100.0, effective_mad, &config));
        // value=115 -> deviation=15, z=3.0 -> anomalous
        assert!(is_value_anomalous(115.0, 100.0, effective_mad, &config));
    }
}
