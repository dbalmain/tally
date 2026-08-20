//! Date filter implementation.

use std::cmp::Reverse;

use chrono::{Datelike, Duration, Months, NaiveDate, Weekday};
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use rusqlite::types::Value;

use crate::search::{Filter, FilterResult, SearchOptions, placeholders as ph};

const STATIC_PRESETS: &[&str] = &[
    "yesterday",
    "last-week",
    "last-month",
    "last-quarter",
    "last-year",
    "last-financial-year",
    "this-week",
    "this-month",
    "this-quarter",
    "this-year",
    "this-financial-year",
];

const RELATIVE_PERIODS: &[&str] = &[
    "days",
    "weeks",
    "months",
    "quarters",
    "years",
    "financial-years",
];

/// Filter for date ranges.
///
/// Supports:
/// - `2024` -> entire year
/// - `2024-01` -> entire month
/// - `2024-01-15` -> exact date
/// - `last-month`, `this-financial-year`, `last-3-months` -> preset ranges
/// - `2y`, `3m`, `1y3m2w1d` -> ago durations from that day onward (open end)
/// - `2y..1y` -> from two years ago to one year ago (endpoint is that day)
/// - `2024-01..last-month` -> explicit range with preset endpoints
/// - `..2024` -> up to end of 2024
/// - `2024..` -> from start of 2024 (open end)
pub struct DateFilter {
    today: NaiveDate,
    week_start: Weekday,
    financial_year_end: (u32, u32),
}

impl DateFilter {
    pub fn new(options: SearchOptions) -> Self {
        Self {
            today: options.today,
            week_start: options.week_start,
            financial_year_end: options.financial_year_end,
        }
    }

    fn parse_range(&self, from: &str, to: &str) -> FilterResult {
        let from_date = if from.is_empty() {
            None
        } else {
            match self.parse_spec(from) {
                Ok((start, _)) => Some(start),
                Err(message) => {
                    return FilterResult::Invalid(format!("Invalid start date: {message}"));
                }
            }
        };

        let to_date = if to.is_empty() {
            None
        } else {
            match self.parse_spec(to) {
                Ok((start, end)) => {
                    // Ago durations are a point (today minus the duration). A
                    // bare `date:2y` is from that day onward; as a range
                    // endpoint `1y` means that day, so `date:2y..1y` is from
                    // two years ago to one year ago.
                    Some(if parse_ago(to).ok().flatten().is_some() {
                        start
                    } else {
                        end
                    })
                }
                Err(message) => {
                    return FilterResult::Invalid(format!("Invalid end date: {message}"));
                }
            }
        };

        match (from_date, to_date) {
            (Some(from), Some(to)) => valid_between(from, to),
            (Some(from), None) => valid_from(from),
            (None, Some(to)) => FilterResult::Valid {
                sql: format!("{} <= ?", ph::reference(ph::DATE)),
                params: vec![Value::Text(to.to_string())],
            },
            (None, None) => FilterResult::Empty,
        }
    }

    fn parse_spec(&self, value: &str) -> Result<(NaiveDate, NaiveDate), String> {
        if let Some(range) = self.parse_preset(value)? {
            return Ok(range);
        }
        parse_date_spec(value).ok_or_else(|| value.to_string())
    }

    fn parse_preset(&self, value: &str) -> Result<Option<(NaiveDate, NaiveDate)>, String> {
        let range = match value {
            "yesterday" => {
                let date = self.today - Duration::days(1);
                Some((date, date))
            }
            "this-week" => Some(self.this_week()),
            "last-week" => Some(self.last_period(1, Period::Weeks)?),
            "this-month" => Some(self.this_month()?),
            "last-month" => Some(self.last_period(1, Period::Months)?),
            "this-quarter" => Some(self.this_quarter()?),
            "last-quarter" => Some(self.last_period(1, Period::Quarters)?),
            "this-year" => Some(self.this_year()?),
            "last-year" => Some(self.last_period(1, Period::Years)?),
            "this-financial-year" => Some(self.this_financial_year()?),
            "last-financial-year" => Some(self.last_period(1, Period::FinancialYears)?),
            _ => {
                if let Some(ago) = parse_ago(value)? {
                    let from = self.ago_anchor(ago)?;
                    Some((from, from))
                } else {
                    match parse_relative(value)? {
                        Some((count, period)) => Some(self.last_period(count, period)?),
                        None => None,
                    }
                }
            }
        };
        Ok(range)
    }

    fn ago_anchor(&self, ago: AgoDuration) -> Result<NaiveDate, String> {
        let mut from = self.today;
        if ago.years > 0 {
            let months = ago
                .years
                .checked_mul(12)
                .ok_or_else(|| "duration year count overflow".to_string())?;
            from = subtract_months(from, months)?;
        }
        if ago.months > 0 {
            from = subtract_months(from, ago.months)?;
        }
        let extra_days = i64::from(ago.weeks)
            .checked_mul(7)
            .and_then(|week_days| week_days.checked_add(i64::from(ago.days)))
            .ok_or_else(|| "duration day count overflow".to_string())?;
        if extra_days > 0 {
            from = from
                .checked_sub_signed(Duration::days(extra_days))
                .ok_or_else(|| "date underflow".to_string())?;
        }
        Ok(from)
    }

    fn this_week(&self) -> (NaiveDate, NaiveDate) {
        let today = self.today.weekday().num_days_from_monday() as i64;
        let start = self.week_start.num_days_from_monday() as i64;
        let days_since_start = (today - start).rem_euclid(7);
        let from = self.today - Duration::days(days_since_start);
        (from, from + Duration::days(6))
    }

    fn this_month(&self) -> Result<(NaiveDate, NaiveDate), String> {
        let from = date(self.today.year(), self.today.month(), 1)?;
        let to = end_of_month(self.today.year(), self.today.month())?;
        Ok((from, to))
    }

    fn this_quarter(&self) -> Result<(NaiveDate, NaiveDate), String> {
        let start_month = quarter_start_month(self.today.month());
        let from = date(self.today.year(), start_month, 1)?;
        let to = add_months(from, 3)?.pred_opt().ok_or("date underflow")?;
        Ok((from, to))
    }

    fn this_year(&self) -> Result<(NaiveDate, NaiveDate), String> {
        Ok((
            date(self.today.year(), 1, 1)?,
            date(self.today.year(), 12, 31)?,
        ))
    }

    fn this_financial_year(&self) -> Result<(NaiveDate, NaiveDate), String> {
        let (month, day) = self.financial_year_end;
        let end_this_year = date(self.today.year(), month, day)?;
        let end_year = if self.today <= end_this_year {
            self.today.year()
        } else {
            self.today
                .year()
                .checked_add(1)
                .ok_or("financial year overflow")?
        };
        let to = date(end_year, month, day)?;
        let previous_end = date(end_year - 1, month, day)?;
        let from = previous_end.succ_opt().ok_or("date overflow")?;
        Ok((from, to))
    }

    fn last_period(&self, count: u32, period: Period) -> Result<(NaiveDate, NaiveDate), String> {
        if count == 0 {
            return Err("last-N period count must be greater than 0".to_string());
        }

        match period {
            Period::Days => {
                let from = self.today - Duration::days(i64::from(count));
                let to = self.today - Duration::days(1);
                Ok((from, to))
            }
            Period::Weeks => {
                let current_start = self.this_week().0;
                let from = current_start - Duration::days(i64::from(count) * 7);
                let to = current_start.pred_opt().ok_or("date underflow")?;
                Ok((from, to))
            }
            Period::Months => {
                let current_start = date(self.today.year(), self.today.month(), 1)?;
                let from = subtract_months(current_start, count)?;
                let to = current_start.pred_opt().ok_or("date underflow")?;
                Ok((from, to))
            }
            Period::Quarters => {
                let start_month = quarter_start_month(self.today.month());
                let current_start = date(self.today.year(), start_month, 1)?;
                let months = count.checked_mul(3).ok_or("quarter count overflow")?;
                let from = subtract_months(current_start, months)?;
                let to = current_start.pred_opt().ok_or("date underflow")?;
                Ok((from, to))
            }
            Period::Years => {
                let current_start = date(self.today.year(), 1, 1)?;
                let from_year = self
                    .today
                    .year()
                    .checked_sub(i32::try_from(count).map_err(|_| "year count overflow")?)
                    .ok_or("year underflow")?;
                let from = date(from_year, 1, 1)?;
                let to = current_start.pred_opt().ok_or("date underflow")?;
                Ok((from, to))
            }
            Period::FinancialYears => {
                let (current_start, _) = self.this_financial_year()?;
                let from_year = current_start
                    .year()
                    .checked_sub(i32::try_from(count).map_err(|_| "financial year count overflow")?)
                    .ok_or("financial year underflow")?;
                let from = date(from_year, current_start.month(), current_start.day())?;
                let to = current_start.pred_opt().ok_or("date underflow")?;
                Ok((from, to))
            }
        }
    }
}

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

        if let Some((from, to)) = value.split_once("..") {
            return self.parse_range(from, to);
        }

        match parse_ago(value) {
            Ok(Some(ago)) => match self.ago_anchor(ago) {
                Ok(from) => valid_from(from),
                Err(message) => FilterResult::Invalid(format!("Invalid date: {message}")),
            },
            Ok(None) => match self.parse_spec(value) {
                Ok((from, to)) => valid_between(from, to),
                Err(message) => FilterResult::Invalid(format!("Invalid date: {message}")),
            },
            Err(message) => FilterResult::Invalid(format!("Invalid date: {message}")),
        }
    }

    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        let (segment, anchor) = range_segment_at_cursor(value, cursor);
        if let Some(suggestions) = ago_completions(segment) {
            return Some((suggestions, anchor));
        }
        if segment.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }

        if segment.is_empty() {
            return Some((
                STATIC_PRESETS
                    .iter()
                    .map(|preset| (*preset).to_string())
                    .collect(),
                anchor,
            ));
        }

        let candidates: Vec<String> = relative_completion_count(segment).map_or_else(
            || {
                STATIC_PRESETS
                    .iter()
                    .map(|preset| (*preset).to_string())
                    .collect::<Vec<_>>()
            },
            |count| {
                RELATIVE_PERIODS
                    .iter()
                    .map(|period| format!("last-{count}-{period}"))
                    .collect::<Vec<_>>()
            },
        );
        let suggestions = fuzzy_rank(&candidates, segment);
        (!suggestions.is_empty()).then_some((suggestions, anchor))
    }

    fn completion_segment_end(&self, value: &str, segment_start: usize) -> usize {
        if let Some(delimiter_byte) = value.find("..") {
            let delimiter = value[..delimiter_byte].chars().count();
            if segment_start <= delimiter {
                return delimiter;
            }
        }

        value.chars().count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Period {
    Days,
    Weeks,
    Months,
    Quarters,
    Years,
    FinancialYears,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct AgoDuration {
    years: u32,
    months: u32,
    weeks: u32,
    days: u32,
}

const AGO_UNITS: &[(u8, i8)] = &[(b'y', 0), (b'm', 1), (b'w', 2), (b'd', 3)];
const AGO_UNITS_ASC: &[(u8, i8)] = &[(b'd', 3), (b'w', 2), (b'm', 1), (b'y', 0)];

fn parse_ago(value: &str) -> Result<Option<AgoDuration>, String> {
    if !looks_like_ago(value) {
        return Ok(None);
    }
    match parse_ago_prefix(value) {
        Some((ago, last_rank, None)) if last_rank >= 0 => Ok(Some(ago)),
        Some((_, _, Some(_))) => Err("duration is missing a unit (y, m, w, or d)".to_string()),
        _ => Err("duration units must be y, m, w, d in that order, each at most once".to_string()),
    }
}

fn looks_like_ago(value: &str) -> bool {
    let s = value.as_bytes();
    !s.is_empty()
        && s[0].is_ascii_digit()
        && s.iter()
            .any(|b| matches!(b, b'y' | b'm' | b'w' | b'd' | b'Y' | b'M' | b'W' | b'D'))
}

/// Walk a compact duration like `1y3m2w1d`.
///
/// Returns `None` when the string is not duration-shaped (so year/month specs
/// and named presets can still match). Returns `Some` with a trailing digit
/// run when the value is an in-progress duration (`1y3`).
fn parse_ago_prefix(value: &str) -> Option<(AgoDuration, i8, Option<&str>)> {
    let s = value.as_bytes();
    if s.is_empty() || !s[0].is_ascii_digit() {
        return None;
    }
    if !s
        .iter()
        .any(|b| matches!(b, b'y' | b'm' | b'w' | b'd' | b'Y' | b'M' | b'W' | b'D'))
    {
        return None;
    }

    let mut i = 0;
    let mut ago = AgoDuration::default();
    let mut last_rank = -1i8;
    while i < s.len() {
        let digit_start = i;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
        if i == digit_start {
            return None;
        }
        if i == s.len() {
            return Some((ago, last_rank, Some(&value[digit_start..])));
        }

        let count: u32 = value[digit_start..i].parse().ok()?;
        if count == 0 {
            return None;
        }

        let unit = s[i].to_ascii_lowercase();
        i += 1;
        let rank = AGO_UNITS.iter().find(|(u, _)| *u == unit)?.1;
        if rank <= last_rank {
            return None;
        }
        last_rank = rank;
        match unit {
            b'y' => ago.years = count,
            b'm' => ago.months = count,
            b'w' => ago.weeks = count,
            b'd' => ago.days = count,
            _ => return None,
        }
    }
    Some((ago, last_rank, None))
}

fn ago_completions(segment: &str) -> Option<Vec<String>> {
    let (ago, last_rank, trailing) = parse_ago_prefix(segment)?;
    let trailing = trailing?;
    if ago == AgoDuration::default() {
        return None;
    }
    if trailing.starts_with('0') {
        return None;
    }
    let count: u32 = trailing.parse().ok()?;
    if count == 0 {
        return None;
    }
    let complete = &segment[..segment.len() - trailing.len()];
    let suggestions: Vec<String> = AGO_UNITS_ASC
        .iter()
        .filter(|(_, rank)| *rank > last_rank)
        .map(|(unit, _)| format!("{complete}{trailing}{}", *unit as char))
        .collect();
    (!suggestions.is_empty()).then_some(suggestions)
}

fn valid_from(from: NaiveDate) -> FilterResult {
    FilterResult::Valid {
        sql: format!("{} >= ?", ph::reference(ph::DATE)),
        params: vec![Value::Text(from.to_string())],
    }
}

fn valid_between(from: NaiveDate, to: NaiveDate) -> FilterResult {
    FilterResult::Valid {
        sql: format!(
            "{} >= ? AND {} <= ?",
            ph::reference(ph::DATE),
            ph::reference(ph::DATE)
        ),
        params: vec![Value::Text(from.to_string()), Value::Text(to.to_string())],
    }
}

fn parse_relative(value: &str) -> Result<Option<(u32, Period)>, String> {
    let Some(rest) = value.strip_prefix("last-") else {
        return Ok(None);
    };
    let Some((count, period)) = rest.split_once('-') else {
        return Ok(None);
    };
    if count.is_empty() || !count.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }

    let count = count
        .parse::<u32>()
        .map_err(|_| format!("invalid last-N period count: {count}"))?;
    if count == 0 {
        return Err("last-N period count must be greater than 0".to_string());
    }

    let period = match period {
        "days" => Period::Days,
        "weeks" => Period::Weeks,
        "months" => Period::Months,
        "quarters" => Period::Quarters,
        "years" => Period::Years,
        "financial-years" => Period::FinancialYears,
        _ => return Err(format!("unknown last-N period: {period}")),
    };
    Ok(Some((count, period)))
}

/// Parse a date spec (year, month, or full date) into a range.
/// Returns (start_date, end_date) inclusive.
pub(crate) fn parse_date_spec(s: &str) -> Option<(NaiveDate, NaiveDate)> {
    let parts: Vec<&str> = s.split('-').collect();

    match parts.len() {
        1 => {
            let year: i32 = parts[0].parse().ok()?;
            let start = NaiveDate::from_ymd_opt(year, 1, 1)?;
            let end = NaiveDate::from_ymd_opt(year, 12, 31)?;
            Some((start, end))
        }
        2 => {
            let year: i32 = parts[0].parse().ok()?;
            let month: u32 = parts[1].parse().ok()?;
            let start = NaiveDate::from_ymd_opt(year, month, 1)?;
            let end = end_of_month(year, month).ok()?;
            Some((start, end))
        }
        3 => {
            let year: i32 = parts[0].parse().ok()?;
            let month: u32 = parts[1].parse().ok()?;
            let day: u32 = parts[2].parse().ok()?;
            let date = NaiveDate::from_ymd_opt(year, month, day)?;
            Some((date, date))
        }
        _ => None,
    }
}

fn date(year: i32, month: u32, day: u32) -> Result<NaiveDate, String> {
    NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| format!("invalid date: {year:04}-{month:02}-{day:02}"))
}

fn end_of_month(year: i32, month: u32) -> Result<NaiveDate, String> {
    if month == 12 {
        date(year, 12, 31)
    } else {
        date(year, month + 1, 1)?
            .pred_opt()
            .ok_or_else(|| "date underflow".to_string())
    }
}

fn add_months(from: NaiveDate, months: u32) -> Result<NaiveDate, String> {
    from.checked_add_months(Months::new(months))
        .ok_or_else(|| "month arithmetic overflow".to_string())
}

fn subtract_months(from: NaiveDate, months: u32) -> Result<NaiveDate, String> {
    from.checked_sub_months(Months::new(months))
        .ok_or_else(|| "month arithmetic underflow".to_string())
}

fn quarter_start_month(month: u32) -> u32 {
    ((month - 1) / 3) * 3 + 1
}

fn range_segment_at_cursor(value: &str, cursor: usize) -> (&str, usize) {
    let Some(delimiter_byte) = value.find("..") else {
        return (value, 0);
    };
    let delimiter = value[..delimiter_byte].chars().count();
    if cursor <= delimiter {
        (char_slice(value, 0, delimiter), 0)
    } else {
        let anchor = delimiter + 2;
        (char_slice(value, anchor, char_len(value)), anchor)
    }
}

fn relative_completion_count(segment: &str) -> Option<&str> {
    let rest = segment.strip_prefix("last-")?;
    let digits_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let suffix = char_slice(rest, digits_len, char_len(rest));
    (suffix.is_empty() || suffix.starts_with('-')).then(|| char_slice(rest, 0, digits_len))
}

fn fuzzy_rank(candidates: &[String], segment: &str) -> Vec<String> {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::new(
        segment,
        CaseMatching::Ignore,
        Normalization::Smart,
        nucleo_matcher::pattern::AtomKind::Fuzzy,
    );

    let mut scored: Vec<(u32, &String)> = candidates
        .iter()
        .filter_map(|candidate| {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(candidate, &mut buf);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (score, candidate))
        })
        .collect();
    scored.sort_by_key(|(score, _)| Reverse(*score));
    scored
        .into_iter()
        .map(|(_, suggestion)| suggestion.clone())
        .collect()
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn char_slice(s: &str, start: usize, end: usize) -> &str {
    let start_byte = char_to_byte_index(s, start);
    let end_byte = char_to_byte_index(s, end);
    &s[start_byte..end_byte]
}

fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> DateFilter {
        filter_with(Weekday::Mon, (6, 30), d(2026, 7, 9))
    }

    fn filter_with(
        week_start: Weekday,
        financial_year_end: (u32, u32),
        today: NaiveDate,
    ) -> DateFilter {
        DateFilter::new(SearchOptions::new(today, week_start, financial_year_end))
    }

    fn parse(value: &str) -> FilterResult {
        filter().parse(value)
    }

    fn d(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn assert_range(filter: &DateFilter, value: &str, from: NaiveDate, to: NaiveDate) {
        match filter.parse(value) {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ? AND {date} <= ?");
                assert_eq!(params[0], Value::Text(from.to_string()));
                assert_eq!(params[1], Value::Text(to.to_string()));
            }
            other => panic!("Expected Valid, got {other:?}"),
        }
    }

    fn assert_from(filter: &DateFilter, value: &str, from: NaiveDate) {
        match filter.parse(value) {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} >= ?");
                assert_eq!(params, vec![Value::Text(from.to_string())]);
            }
            other => panic!("Expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn test_empty() {
        assert!(matches!(parse(""), FilterResult::Empty));
    }

    #[test]
    fn test_year() {
        assert_range(&filter(), "2024", d(2024, 1, 1), d(2024, 12, 31));
    }

    #[test]
    fn test_month() {
        assert_range(&filter(), "2024-02", d(2024, 2, 1), d(2024, 2, 29));
    }

    #[test]
    fn test_exact_date() {
        assert_range(&filter(), "2024-01-15", d(2024, 1, 15), d(2024, 1, 15));
    }

    #[test]
    fn test_range() {
        assert_range(&filter(), "2024-01..2024-06", d(2024, 1, 1), d(2024, 6, 30));
    }

    #[test]
    fn test_open_end_range() {
        assert_from(&filter(), "2024..", d(2024, 1, 1));
    }

    #[test]
    fn test_open_start_range() {
        match parse("..2024") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} <= ?");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0], Value::Text("2024-12-31".to_string()));
            }
            other => panic!("Expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn presets_expand_to_full_periods() {
        let filter = filter();
        for (value, from, to) in [
            ("yesterday", d(2026, 7, 8), d(2026, 7, 8)),
            ("this-week", d(2026, 7, 6), d(2026, 7, 12)),
            ("last-week", d(2026, 6, 29), d(2026, 7, 5)),
            ("this-month", d(2026, 7, 1), d(2026, 7, 31)),
            ("last-month", d(2026, 6, 1), d(2026, 6, 30)),
            ("this-quarter", d(2026, 7, 1), d(2026, 9, 30)),
            ("last-quarter", d(2026, 4, 1), d(2026, 6, 30)),
            ("this-year", d(2026, 1, 1), d(2026, 12, 31)),
            ("last-year", d(2025, 1, 1), d(2025, 12, 31)),
            ("this-financial-year", d(2026, 7, 1), d(2027, 6, 30)),
            ("last-financial-year", d(2025, 7, 1), d(2026, 6, 30)),
        ] {
            assert_range(&filter, value, from, to);
        }
    }

    #[test]
    fn week_start_changes_week_presets() {
        let filter = filter_with(Weekday::Sun, (6, 30), d(2026, 7, 9));

        assert_range(&filter, "this-week", d(2026, 7, 5), d(2026, 7, 11));
        assert_range(&filter, "last-week", d(2026, 6, 28), d(2026, 7, 4));
    }

    #[test]
    fn financial_year_boundaries_include_year_end_and_start_new_year_after() {
        let on_end = filter_with(Weekday::Mon, (6, 30), d(2026, 6, 30));
        assert_range(
            &on_end,
            "this-financial-year",
            d(2025, 7, 1),
            d(2026, 6, 30),
        );
        assert_range(
            &on_end,
            "last-financial-year",
            d(2024, 7, 1),
            d(2025, 6, 30),
        );

        let on_start = filter_with(Weekday::Mon, (6, 30), d(2026, 7, 1));
        assert_range(
            &on_start,
            "this-financial-year",
            d(2026, 7, 1),
            d(2027, 6, 30),
        );
        assert_range(
            &on_start,
            "last-financial-year",
            d(2025, 7, 1),
            d(2026, 6, 30),
        );
    }

    #[test]
    fn relative_last_n_periods_exclude_current_period() {
        let filter = filter();
        for (value, from, to) in [
            ("last-1-days", d(2026, 7, 8), d(2026, 7, 8)),
            ("last-7-days", d(2026, 7, 2), d(2026, 7, 8)),
            ("last-2-weeks", d(2026, 6, 22), d(2026, 7, 5)),
            ("last-3-months", d(2026, 4, 1), d(2026, 6, 30)),
            ("last-2-quarters", d(2026, 1, 1), d(2026, 6, 30)),
            ("last-2-years", d(2024, 1, 1), d(2025, 12, 31)),
            ("last-2-financial-years", d(2024, 7, 1), d(2026, 6, 30)),
        ] {
            assert_range(&filter, value, from, to);
        }
    }

    #[test]
    fn ago_durations_count_back_from_today_open_ended() {
        let filter = filter_with(Weekday::Mon, (6, 30), d(2026, 8, 21));
        for (value, from) in [
            ("2y", d(2024, 8, 21)),
            ("2Y", d(2024, 8, 21)),
            ("2m", d(2026, 6, 21)),
            ("2w", d(2026, 8, 7)),
            ("1d", d(2026, 8, 20)),
            ("1y3m2w1d", d(2025, 5, 6)),
        ] {
            assert_from(&filter, value, from);
        }
    }

    #[test]
    fn ago_months_clamp_to_end_of_shorter_month() {
        let filter = filter_with(Weekday::Mon, (6, 30), d(2026, 3, 31));
        assert_from(&filter, "1m", d(2026, 2, 28));

        let leap = filter_with(Weekday::Mon, (6, 30), d(2024, 2, 29));
        assert_from(&leap, "1y", d(2023, 2, 28));
    }

    #[test]
    fn ago_durations_work_as_range_endpoints() {
        let filter = filter_with(Weekday::Mon, (6, 30), d(2026, 8, 21));
        assert_range(&filter, "2y..1y", d(2024, 8, 21), d(2025, 8, 21));
        assert_range(&filter, "2m..1m", d(2026, 6, 21), d(2026, 7, 21));
        assert_range(&filter, "2m..yesterday", d(2026, 6, 21), d(2026, 8, 20));
        assert_range(&filter, "2026-01-01..2m", d(2026, 1, 1), d(2026, 6, 21));
        assert_from(&filter, "2y..", d(2024, 8, 21));
        match filter.parse("..1y") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "{date} <= ?");
                assert_eq!(params[0], Value::Text("2025-08-21".to_string()));
            }
            other => panic!("Expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn presets_work_as_range_endpoints() {
        assert_range(
            &filter(),
            "last-quarter..yesterday",
            d(2026, 4, 1),
            d(2026, 7, 8),
        );
        assert_range(
            &filter(),
            "2026-01..last-month",
            d(2026, 1, 1),
            d(2026, 6, 30),
        );
    }

    #[test]
    fn test_invalid() {
        assert!(matches!(parse("invalid"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2024-13"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2024-01-32"), FilterResult::Invalid(_)));
        assert!(matches!(parse("last-0-days"), FilterResult::Invalid(_)));
        assert!(matches!(
            parse("last-3-fortnights"),
            FilterResult::Invalid(_)
        ));
        assert!(matches!(parse("lastweek"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2d1y"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2y3"), FilterResult::Invalid(_)));
        assert!(matches!(parse("0d"), FilterResult::Invalid(_)));
        assert!(matches!(parse("2yy"), FilterResult::Invalid(_)));
    }

    #[test]
    fn completions_empty_segment_returns_all_presets() {
        let (suggestions, anchor) = filter().completions("", 0).unwrap();

        assert_eq!(anchor, 0);
        assert_eq!(
            suggestions,
            STATIC_PRESETS
                .iter()
                .map(|preset| (*preset).to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn completions_digit_segment_returns_none() {
        assert!(filter().completions("2", 1).is_none());
        assert!(filter().completions("..2", 3).is_none());
        assert!(filter().completions("2024", 4).is_none());
        assert!(filter().completions("2y", 2).is_none());
    }

    #[test]
    fn completions_ago_prefix_suggests_remaining_units() {
        let (suggestions, anchor) = filter().completions("1y3", 3).unwrap();
        assert_eq!(anchor, 0);
        assert_eq!(suggestions, vec!["1y3d", "1y3w", "1y3m"]);

        let (suggestions, anchor) = filter().completions("..2m1", 5).unwrap();
        assert_eq!(anchor, 2);
        assert_eq!(suggestions, vec!["2m1d", "2m1w"]);
    }

    #[test]
    fn completions_relative_prefix_returns_dynamic_periods() {
        let (suggestions, anchor) = filter().completions("last-3", 6).unwrap();

        assert_eq!(anchor, 0);
        assert_eq!(
            suggestions,
            vec![
                "last-3-days",
                "last-3-weeks",
                "last-3-months",
                "last-3-quarters",
                "last-3-years",
                "last-3-financial-years",
            ]
        );
    }

    #[test]
    fn completions_right_side_of_range_use_right_anchor() {
        let (suggestions, anchor) = filter().completions("..las", 5).unwrap();

        assert_eq!(anchor, 2);
        assert!(suggestions.iter().any(|s| s == "last-week"));
    }
}
