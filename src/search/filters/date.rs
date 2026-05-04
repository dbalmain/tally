//! Date filter implementation.

use chrono::NaiveDate;
use rusqlite::types::Value;

use crate::search::{Filter, FilterResult};

/// Filter for date ranges.
///
/// Supports:
/// - `2024` → entire year
/// - `2024-01` → entire month
/// - `2024-01-15` → exact date
/// - `2024-01..2024-06` → explicit range (inclusive)
/// - `..2024` → up to end of 2024
/// - `2024..` → from start of 2024
pub struct DateFilter;

impl Filter for DateFilter {
    fn name(&self) -> &'static str {
        "date"
    }

    fn alias(&self) -> Option<&'static str> {
        Some("d")
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        // Check for range syntax
        if let Some((from, to)) = value.split_once("..") {
            return self.parse_range(from, to);
        }

        // Single value - parse as date/month/year and expand to range
        match parse_date_spec(value) {
            Some((from, to)) => FilterResult::Valid {
                sql: "{date} >= ? AND {date} <= ?".to_string(),
                params: vec![Value::Text(from.to_string()), Value::Text(to.to_string())],
            },
            None => FilterResult::Invalid(format!("Invalid date: {}", value)),
        }
    }
}

impl DateFilter {
    fn parse_range(&self, from: &str, to: &str) -> FilterResult {
        let from_date = if from.is_empty() {
            None
        } else {
            match parse_date_spec(from) {
                Some((start, _)) => Some(start),
                None => return FilterResult::Invalid(format!("Invalid start date: {}", from)),
            }
        };

        let to_date = if to.is_empty() {
            None
        } else {
            match parse_date_spec(to) {
                Some((_, end)) => Some(end),
                None => return FilterResult::Invalid(format!("Invalid end date: {}", to)),
            }
        };

        match (from_date, to_date) {
            (Some(from), Some(to)) => FilterResult::Valid {
                sql: "{date} >= ? AND {date} <= ?".to_string(),
                params: vec![Value::Text(from.to_string()), Value::Text(to.to_string())],
            },
            (Some(from), None) => FilterResult::Valid {
                sql: "{date} >= ?".to_string(),
                params: vec![Value::Text(from.to_string())],
            },
            (None, Some(to)) => FilterResult::Valid {
                sql: "{date} <= ?".to_string(),
                params: vec![Value::Text(to.to_string())],
            },
            (None, None) => FilterResult::Empty,
        }
    }
}

/// Parse a date spec (year, month, or full date) into a range.
/// Returns (start_date, end_date) inclusive.
pub(crate) fn parse_date_spec(s: &str) -> Option<(NaiveDate, NaiveDate)> {
    let parts: Vec<&str> = s.split('-').collect();

    match parts.len() {
        1 => {
            // Year only: 2024
            let year: i32 = parts[0].parse().ok()?;
            let start = NaiveDate::from_ymd_opt(year, 1, 1)?;
            let end = NaiveDate::from_ymd_opt(year, 12, 31)?;
            Some((start, end))
        }
        2 => {
            // Year-month: 2024-01
            let year: i32 = parts[0].parse().ok()?;
            let month: u32 = parts[1].parse().ok()?;
            let start = NaiveDate::from_ymd_opt(year, month, 1)?;
            // Last day of month
            let end = if month == 12 {
                NaiveDate::from_ymd_opt(year, 12, 31)?
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)?.pred_opt()?
            };
            Some((start, end))
        }
        3 => {
            // Full date: 2024-01-15
            let year: i32 = parts[0].parse().ok()?;
            let month: u32 = parts[1].parse().ok()?;
            let day: u32 = parts[2].parse().ok()?;
            let date = NaiveDate::from_ymd_opt(year, month, day)?;
            Some((date, date))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> FilterResult {
        DateFilter.parse(value)
    }

    #[test]
    fn test_empty() {
        assert!(matches!(parse(""), FilterResult::Empty));
    }

    #[test]
    fn test_year() {
        match parse("2024") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ? AND {date} <= ?");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0], Value::Text("2024-01-01".to_string()));
                assert_eq!(params[1], Value::Text("2024-12-31".to_string()));
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_month() {
        match parse("2024-02") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ? AND {date} <= ?");
                assert_eq!(params[0], Value::Text("2024-02-01".to_string()));
                assert_eq!(params[1], Value::Text("2024-02-29".to_string())); // leap year
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_exact_date() {
        match parse("2024-01-15") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ? AND {date} <= ?");
                assert_eq!(params[0], Value::Text("2024-01-15".to_string()));
                assert_eq!(params[1], Value::Text("2024-01-15".to_string()));
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_range() {
        match parse("2024-01..2024-06") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ? AND {date} <= ?");
                assert_eq!(params[0], Value::Text("2024-01-01".to_string()));
                assert_eq!(params[1], Value::Text("2024-06-30".to_string()));
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_open_end_range() {
        match parse("2024..") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ?");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0], Value::Text("2024-01-01".to_string()));
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_open_start_range() {
        match parse("..2024") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} <= ?");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0], Value::Text("2024-12-31".to_string()));
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_invalid() {
        assert!(matches!(parse("invalid"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2024-13"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2024-01-32"), FilterResult::Invalid(_)));
    }
}
