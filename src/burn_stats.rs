//! Per-day burn statistics (`burn_stats.json`).
//!
//! Official AiBurn keeps per-day success/failure counters behind the
//! `show_statistic` / `db_inited` `AiBurn.ini` keys. This module provides the
//! same shape without extra dependencies (`serde_json` is already used):
//!
//! ```json
//! {"days": {"2026-10-09": {"success": 2, "failure": 1, "cancelled": 0}}}
//! ```
//!
//! Dates are UTC `YYYY-MM-DD` (no date crate; Howard Hinnant's civil-from-days,
//! same algorithm as [`crate::services::log_file_stamp`]). A missing or corrupt
//! file loads as empty rather than failing the burn flow.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Burn outcome recorded into the daily counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BurnOutcome {
    Success,
    Failure,
    Cancelled,
}

/// Counters for a single UTC day.
#[derive(Clone, Debug, Default)]
pub struct DayStat {
    pub success: u64,
    pub failure: u64,
    pub cancelled: u64,
}

impl DayStat {
    pub fn total(&self) -> u64 {
        self.success + self.failure + self.cancelled
    }

    /// Success rate over non-cancelled attempts, in `[0.0, 1.0]`.
    /// Returns `None` when there were no success/failure attempts.
    pub fn success_rate(&self) -> Option<f64> {
        let denom = self.success + self.failure;
        if denom == 0 {
            None
        } else {
            Some(self.success as f64 / denom as f64)
        }
    }
}

/// Whole statistics file.
#[derive(Clone, Debug, Default)]
pub struct BurnStats {
    pub days: BTreeMap<String, DayStat>,
}

impl BurnStats {
    pub fn total(&self) -> DayStat {
        let mut total = DayStat::default();
        for day in self.days.values() {
            total.success += day.success;
            total.failure += day.failure;
            total.cancelled += day.cancelled;
        }
        total
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut days = serde_json::Map::new();
        for (date, stat) in &self.days {
            days.insert(
                date.clone(),
                serde_json::json!({
                    "success": stat.success,
                    "failure": stat.failure,
                    "cancelled": stat.cancelled,
                }),
            );
        }
        serde_json::json!({ "days": days })
    }
}

pub fn stats_path(app_dir: &Path) -> PathBuf {
    app_dir.join("burn_stats.json")
}

/// Current UTC day as `YYYY-MM-DD`.
pub fn today_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    date_stamp(secs)
}

/// UTC `YYYY-MM-DD` for Unix `secs` (no date dependency).
pub fn date_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", year, m, d)
}

/// Classify a burn error string: user cancellations (device error
/// `"Burn cancelled by user"`) count separately from real failures.
pub fn classify_error(err: &str) -> BurnOutcome {
    let lower = err.to_ascii_lowercase();
    if lower.contains("cancell") || lower.contains("cancelled") || err.contains("停止烧录") {
        BurnOutcome::Cancelled
    } else {
        BurnOutcome::Failure
    }
}

/// Load stats; missing/corrupt files yield empty stats (never fail the caller).
pub fn load(app_dir: &Path) -> BurnStats {
    let path = stats_path(app_dir);
    let Ok(text) = fs::read_to_string(&path) else {
        return BurnStats::default();
    };
    parse(&text)
}

fn parse(text: &str) -> BurnStats {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return BurnStats::default();
    };
    // Accept both `{"days": {...}}` (current) and a bare `{date: {...}}` map.
    let days_value = value
        .get("days")
        .cloned()
        .unwrap_or_else(|| value.clone());
    let Some(map) = days_value.as_object() else {
        return BurnStats::default();
    };
    let mut stats = BurnStats::default();
    for (date, entry) in map {
        if !is_date_key(date) {
            continue;
        }
        stats.days.insert(
            date.clone(),
            DayStat {
                success: entry.get("success").and_then(|v| v.as_u64()).unwrap_or(0),
                failure: entry.get("failure").and_then(|v| v.as_u64()).unwrap_or(0),
                cancelled: entry
                    .get("cancelled")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            },
        );
    }
    stats
}

fn is_date_key(key: &str) -> bool {
    // Minimal `YYYY-MM-DD` shape check ( avoids pulling date-like junk in ).
    let bytes = key.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && key[..4].chars().all(|c| c.is_ascii_digit())
        && key[5..7].chars().all(|c| c.is_ascii_digit())
        && key[8..].chars().all(|c| c.is_ascii_digit())
}

pub fn save(app_dir: &Path, stats: &BurnStats) -> Result<(), String> {
    let path = stats_path(app_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
    }
    let text = serde_json::to_string_pretty(&stats.to_json())
        .map_err(|e| format!("Failed to encode burn stats: {}", e))?;
    fs::write(&path, format!("{}\n", text))
        .map_err(|e| format!("Failed to write '{}': {}", path.display(), e))
}

/// Record one outcome for today and persist. Never fails the burn flow with
/// more than a warning string.
pub fn record(app_dir: &Path, outcome: BurnOutcome) -> Result<BurnStats, String> {
    let mut stats = load(app_dir);
    let entry = stats.days.entry(today_stamp()).or_default();
    match outcome {
        BurnOutcome::Success => entry.success += 1,
        BurnOutcome::Failure => entry.failure += 1,
        BurnOutcome::Cancelled => entry.cancelled += 1,
    }
    save(app_dir, &stats)?;
    Ok(stats)
}

pub fn clear(app_dir: &Path) -> Result<(), String> {
    save(app_dir, &BurnStats::default())
}

/// Human-readable table shared by CLI `stats` and the GUI settings page.
pub fn format_table(stats: &BurnStats) -> String {
    if stats.days.is_empty() {
        return "No burn statistics yet.".to_string();
    }
    let mut lines = vec!["date        success failure cancelled total success_rate".to_string()];
    for (date, day) in &stats.days {
        let rate = day
            .success_rate()
            .map(|r| format!("{:5.1}%", r * 100.0))
            .unwrap_or_else(|| "   --".to_string());
        lines.push(format!(
            "{} {:7} {:7} {:9} {:5} {}",
            date,
            day.success,
            day.failure,
            day.cancelled,
            day.total(),
            rate
        ));
    }
    let total = stats.total();
    let total_rate = total
        .success_rate()
        .map(|r| format!("{:.1}%", r * 100.0))
        .unwrap_or_else(|| "--".to_string());
    lines.push(format!(
        "total       {:7} {:7} {:9} {:5} {}",
        total.success,
        total.failure,
        total.cancelled,
        total.total(),
        total_rate
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_stamp_uses_utc_calendar() {
        assert_eq!(date_stamp(0), "1970-01-01");
        // 2024-01-01 00:00:00 UTC
        assert_eq!(date_stamp(1_704_067_200), "2024-01-01");
        assert_eq!(date_stamp(1_704_067_200 + 86_400), "2024-01-02");
    }

    #[test]
    fn classify_error_separates_cancel_from_failure() {
        assert_eq!(
            classify_error("Burn cancelled by user"),
            BurnOutcome::Cancelled
        );
        assert_eq!(classify_error("CANCELLED"), BurnOutcome::Cancelled);
        assert_eq!(
            classify_error("Bootloader probe after reconnect failed: timeout"),
            BurnOutcome::Failure
        );
        assert_eq!(classify_error(""), BurnOutcome::Failure);
    }

    #[test]
    fn corrupt_file_loads_as_empty() {
        assert!(parse("not json").days.is_empty());
        assert!(parse("{}").days.is_empty());
        assert!(parse("[]").days.is_empty());
    }

    #[test]
    fn parse_accepts_wrapped_and_bare_shapes() {
        let wrapped = parse(
            r#"{"days": {"2026-10-09": {"success": 2, "failure": 1}}}"#,
        );
        assert_eq!(wrapped.days["2026-10-09"].success, 2);
        assert_eq!(wrapped.days["2026-10-09"].failure, 1);
        assert_eq!(wrapped.days["2026-10-09"].cancelled, 0);
        let bare = parse(r#"{"2026-10-09": {"success": 1}}"#);
        assert_eq!(bare.days["2026-10-09"].success, 1);
        // Non-date keys are ignored.
        let junk = parse(r#"{"days": {"notes": {"success": 9}}}"#);
        assert!(junk.days.is_empty());
    }

    #[test]
    fn record_round_trips_through_temp_dir() {
        let unique = format!(
            "artinchip-flash-stats-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        record(&dir, BurnOutcome::Success).unwrap();
        record(&dir, BurnOutcome::Failure).unwrap();
        record(&dir, BurnOutcome::Cancelled).unwrap();
        let stats = load(&dir);
        let today = today_stamp();
        assert_eq!(stats.days[&today].success, 1);
        assert_eq!(stats.days[&today].failure, 1);
        assert_eq!(stats.days[&today].cancelled, 1);
        assert!(format_table(&stats).contains(&today));
        clear(&dir).unwrap();
        assert!(load(&dir).days.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn success_rate_ignores_cancelled() {
        let day = DayStat {
            success: 3,
            failure: 1,
            cancelled: 10,
        };
        assert!((day.success_rate().unwrap() - 0.75).abs() < 1e-9);
        assert!(DayStat::default().success_rate().is_none());
    }
}
