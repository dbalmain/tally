use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

pub struct FuzzyMatcher {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyMatcher {
    /// Create a new fuzzy matcher instance.
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(nucleo_matcher::Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Score a pattern against haystack text, returning None if no match.
    pub fn score(&mut self, pattern: &str, haystack: &str) -> Option<u32> {
        if pattern.is_empty() {
            return Some(0);
        }
        let pat = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);
        self.buf.clear();
        let haystack = Utf32Str::new(haystack, &mut self.buf);
        pat.score(haystack, &mut self.matcher)
    }

    /// Check if pattern fuzzy-matches haystack.
    pub fn fuzzy_matches(&mut self, pattern: &str, haystack: &str) -> bool {
        if pattern.is_empty() {
            return true;
        }
        self.score(pattern, haystack).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_matcher() {
        let mut m = FuzzyMatcher::new();
        assert!(m.fuzzy_matches("ctysd", "CITYSIDE BANK"));
        assert!(m.fuzzy_matches("ctysd", "cityside"));
        assert!(m.fuzzy_matches("", "anything"));
        assert!(!m.fuzzy_matches("xyz", "cityside"));
    }
}
