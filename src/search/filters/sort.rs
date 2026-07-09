//! Sort pseudo-filter implementation.

use std::cmp::Reverse;
use std::collections::HashSet;

use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::search::{Filter, FilterResult, SortColumn, SortKey};

/// Sort term for transaction DB searches.
pub struct SortFilter;

impl Filter for SortFilter {
    fn name(&self) -> &'static str {
        "sort"
    }

    fn parse(&self, value: &str) -> FilterResult {
        match parse_sort_keys(value) {
            Ok(keys) if keys.is_empty() => FilterResult::Empty,
            Ok(_) => FilterResult::Valid {
                sql: String::new(),
                params: Vec::new(),
            },
            Err(message) => FilterResult::Invalid(message),
        }
    }

    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        complete_sort_segments(value, cursor)
    }

    fn sort_keys(&self, value: &str) -> Option<Result<Vec<SortKey>, String>> {
        Some(parse_sort_keys(value))
    }

    fn completion_segment_end(&self, value: &str, segment_start: usize) -> usize {
        value
            .chars()
            .enumerate()
            .skip(segment_start)
            .find_map(|(idx, c)| (c == ',').then_some(idx))
            .unwrap_or_else(|| value.chars().count())
    }
}

fn parse_sort_keys(value: &str) -> Result<Vec<SortKey>, String> {
    value
        .split(',')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let (descending, name) = segment
                .strip_prefix('-')
                .map_or((false, segment), |name| (true, name));
            let Some(column) = SortColumn::from_name(name) else {
                return Err(format!("Unknown sort column: {name}"));
            };
            Ok(SortKey { column, descending })
        })
        .collect()
}

fn complete_sort_segments(value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
    let segments: Vec<(usize, &str)> = value
        .split(',')
        .scan(0, |pos, segment| {
            let start = *pos;
            *pos += segment.chars().count() + 1;
            Some((start, segment))
        })
        .collect();

    let (anchor_offset, current_segment) = segments
        .iter()
        .find(|(start, segment)| cursor >= *start && cursor <= start + segment.chars().count())
        .map(|(start, segment)| (*start, *segment))
        .unwrap_or((0, value));

    let used: HashSet<&str> = segments
        .iter()
        .filter(|(start, _)| *start != anchor_offset)
        .map(|(_, segment)| segment.strip_prefix('-').unwrap_or(segment))
        .filter(|segment| SortColumn::from_name(segment).is_some())
        .collect();

    let descending = current_segment.starts_with('-');
    let needle = current_segment.strip_prefix('-').unwrap_or(current_segment);
    let candidates: Vec<String> = SortColumn::ALL
        .iter()
        .map(|column| column.name())
        .filter(|name| !used.contains(name))
        .map(|name| {
            if descending {
                format!("-{name}")
            } else {
                name.to_string()
            }
        })
        .collect();

    let suggestions = if needle.is_empty() {
        candidates
    } else {
        fuzzy_rank(&candidates, needle, descending)
    };

    (!suggestions.is_empty()).then_some((suggestions, anchor_offset))
}

fn fuzzy_rank(candidates: &[String], needle: &str, descending: bool) -> Vec<String> {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::new(
        needle,
        CaseMatching::Ignore,
        Normalization::Smart,
        nucleo_matcher::pattern::AtomKind::Fuzzy,
    );

    let mut scored: Vec<(u32, &String)> = candidates
        .iter()
        .filter_map(|candidate| {
            let haystack_text = candidate
                .as_str()
                .strip_prefix('-')
                .unwrap_or(candidate.as_str());
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(haystack_text, &mut buf);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (score, candidate))
        })
        .collect();

    scored.sort_by_key(|score| Reverse(score.0));
    scored
        .into_iter()
        .map(|(_, suggestion)| {
            if descending && !suggestion.starts_with('-') {
                format!("-{suggestion}")
            } else {
                suggestion.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_column_and_descending() {
        assert_eq!(
            parse_sort_keys("category,-amount").unwrap(),
            vec![
                SortKey {
                    column: SortColumn::Category,
                    descending: false,
                },
                SortKey {
                    column: SortColumn::Amount,
                    descending: true,
                },
            ]
        );
    }

    #[test]
    fn rejects_unknown_column() {
        assert_eq!(
            parse_sort_keys("date,nope").unwrap_err(),
            "Unknown sort column: nope"
        );
    }

    #[test]
    fn completions_empty_segment_returns_all_columns() {
        let (suggestions, anchor) = complete_sort_segments("", 0).unwrap();
        assert_eq!(anchor, 0);
        assert_eq!(
            suggestions,
            vec![
                "date",
                "description",
                "amount",
                "balance",
                "account",
                "bank",
                "category",
            ]
        );
    }

    #[test]
    fn completions_keep_descending_prefix() {
        let (suggestions, anchor) = complete_sort_segments("-am", 3).unwrap();
        assert_eq!(anchor, 0);
        assert_eq!(suggestions[0], "-amount");
    }

    #[test]
    fn completions_exclude_used_columns() {
        let (suggestions, anchor) = complete_sort_segments("date,am", 7).unwrap();
        assert_eq!(anchor, 5);
        assert!(!suggestions.iter().any(|suggestion| suggestion == "date"));
        assert!(suggestions.iter().any(|suggestion| suggestion == "amount"));
    }
}
