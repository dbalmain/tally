use super::tfidf::{SparseRow, Tfidf, l2_normalise};

/// Default cosine-similarity cutoff for "strong" description matches.
/// Tunable; descriptions that are near-identical score ~0.8-1.0, while a shared
/// merchant with differing detail tokens scores well below this.
pub const SIMILARITY_THRESHOLD: f32 = 0.6;

/// A TF-IDF index over candidate transactions' normalised descriptions,
/// precomputed so similarity queries are a sparse dot product.
pub struct SimilarityIndex {
    tfidf: Tfidf,
    rows: Vec<(i64, SparseRow)>,
}

impl SimilarityIndex {
    /// Build from candidates `(id, normalised_description)` plus extra corpus
    /// strings (e.g. confirmed examples' norms) used only to enrich the
    /// vocabulary/IDF. Only `candidates` get searchable rows.
    pub fn build(candidates: &[(i64, String)], extra_corpus: &[String]) -> Option<Self> {
        let documents: Vec<&str> = candidates
            .iter()
            .map(|(_, norm)| norm.as_str())
            .chain(extra_corpus.iter().map(String::as_str))
            .collect();
        let tfidf = Tfidf::fit(&documents)?;
        let rows = candidates
            .iter()
            .map(|(id, norm)| {
                let mut row = tfidf.transform(norm);
                l2_normalise(&mut row);
                (*id, row)
            })
            .collect();
        Some(Self { tfidf, rows })
    }

    /// Candidates with cosine similarity >= `threshold` to `query_norm`,
    /// excluding `exclude_id`, sorted by descending similarity.
    pub fn similar_to(&self, query_norm: &str, exclude_id: i64, threshold: f32) -> Vec<(i64, f32)> {
        let mut query = self.tfidf.transform(query_norm);
        l2_normalise(&mut query);
        let mut matches: Vec<_> = self
            .rows
            .iter()
            .filter_map(|(id, row)| {
                if *id == exclude_id {
                    return None;
                }
                let score = dot(&query, row);
                (score >= threshold).then_some((*id, score))
            })
            .collect();
        matches.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        matches
    }
}

fn dot(a: &SparseRow, b: &SparseRow) -> f32 {
    let mut left = 0;
    let mut right = 0;
    let mut total = 0.0;

    while let (Some((left_index, left_value)), Some((right_index, right_value))) =
        (a.get(left), b.get(right))
    {
        match left_index.cmp(right_index) {
            std::cmp::Ordering::Less => left += 1,
            std::cmp::Ordering::Greater => right += 1,
            std::cmp::Ordering::Equal => {
                total += left_value * right_value;
                left += 1;
                right += 1;
            }
        }
    }

    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: i64, norm: &str) -> (i64, String) {
        (id, norm.to_string())
    }

    #[test]
    fn near_identical_descriptions_are_returned() {
        let candidates = vec![
            candidate(1, "coffee shop surry hills"),
            candidate(2, "coffee shop surry hill"),
        ];
        let index = SimilarityIndex::build(&candidates, &[]).unwrap();

        let matches = index.similar_to("coffee shop surry hills", 1, SIMILARITY_THRESHOLD);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, 2);
        assert!(matches[0].1 >= SIMILARITY_THRESHOLD);
    }

    #[test]
    fn detail_tokens_separate_same_merchant_transactions() {
        let candidates = vec![
            candidate(1, "rates property a"),
            candidate(2, "rates property a"),
            candidate(3, "rates property b"),
            candidate(4, "rates property b"),
        ];
        let extra = vec!["rates property a".to_string()];
        let index = SimilarityIndex::build(&candidates, &extra).unwrap();

        let matches = index.similar_to("rates property a", 1, SIMILARITY_THRESHOLD);
        let ids: Vec<_> = matches.into_iter().map(|(id, _)| id).collect();

        assert_eq!(ids, vec![2]);
    }

    #[test]
    fn exclude_id_is_never_returned() {
        let candidates = vec![candidate(1, "coffee shop"), candidate(2, "coffee shop")];
        let index = SimilarityIndex::build(&candidates, &[]).unwrap();

        let matches = index.similar_to("coffee shop", 1, SIMILARITY_THRESHOLD);

        assert_eq!(
            matches.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(!matches.iter().any(|(id, _)| *id == 1));
    }

    #[test]
    fn l2_normalise_normalises_known_sparse_row() {
        let mut row = vec![(3, 3.0), (7, 4.0)];

        l2_normalise(&mut row);

        assert_eq!(row.len(), 2);
        assert_eq!(row[0].0, 3);
        assert_eq!(row[1].0, 7);
        assert!((row[0].1 - 0.6).abs() < 1e-6);
        assert!((row[1].1 - 0.8).abs() < 1e-6);
    }
}
