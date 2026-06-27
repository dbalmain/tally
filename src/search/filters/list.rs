//! Shared helpers for pipe-separated list filters.

use std::cmp::Reverse;

use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use rusqlite::types::Value;

use crate::search::FilterResult;

pub(super) fn parse_pipe_segments<F>(value: &str, mut parse_segment: F) -> FilterResult
where
    F: FnMut(&str) -> Result<(String, Vec<Value>), String>,
{
    if value.is_empty() {
        return FilterResult::Empty;
    }

    let mut clauses = Vec::new();
    let mut params = Vec::new();

    for segment in value.split('|').filter(|segment| !segment.is_empty()) {
        let (clause, mut segment_params) = match parse_segment(segment) {
            Ok(parsed) => parsed,
            Err(message) => return FilterResult::Invalid(message),
        };
        clauses.push(clause);
        params.append(&mut segment_params);
    }

    let sql = match clauses.len() {
        0 => return FilterResult::Empty,
        1 => clauses.remove(0),
        _ => format!("({})", clauses.join(" OR ")),
    };

    FilterResult::Valid { sql, params }
}

pub(super) fn complete_pipe_segments(
    options: &[String],
    value: &str,
    cursor: usize,
) -> Option<(Vec<String>, usize)> {
    let segments: Vec<(usize, &str)> = value
        .split('|')
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

    let other_segments: Vec<&str> = segments
        .iter()
        .filter(|(start, _)| *start != anchor_offset)
        .map(|(_, segment)| *segment)
        .collect();

    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::new(
        current_segment,
        CaseMatching::Ignore,
        Normalization::Smart,
        nucleo_matcher::pattern::AtomKind::Fuzzy,
    );

    let mut scored: Vec<(u32, &String)> = options
        .iter()
        .filter(|option| !other_segments.contains(&option.as_str()))
        .filter_map(|option| {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(option, &mut buf);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (score, option))
        })
        .collect();

    scored.sort_by_key(|score| Reverse(score.0));

    let suggestions: Vec<String> = scored
        .into_iter()
        .map(|(_, suggestion)| suggestion.clone())
        .collect();

    (!suggestions.is_empty()).then_some((suggestions, anchor_offset))
}
