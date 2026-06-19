use std::collections::{HashMap, HashSet};

const MIN_DF: usize = 2;

pub(super) type SparseRow = Vec<(usize, f32)>;

pub(super) struct Tfidf {
    word: Vocabulary,
    chars: Vocabulary,
}

struct Vocabulary {
    analyzer: Analyzer,
    offset: usize,
    indices: HashMap<String, usize>,
    idf: Vec<f32>,
}

#[derive(Clone, Copy)]
enum Analyzer {
    Word,
    CharWb,
}

impl Tfidf {
    pub(super) fn fit(documents: &[&str]) -> Option<Self> {
        if documents.is_empty() {
            return None;
        }
        let word = Vocabulary::fit(documents, Analyzer::Word, 0);
        let chars = Vocabulary::fit(documents, Analyzer::CharWb, word.len());
        if word.len() + chars.len() == 0 {
            None
        } else {
            Some(Self { word, chars })
        }
    }

    pub(super) fn len(&self) -> usize {
        self.word.len() + self.chars.len()
    }

    pub(super) fn transform(&self, document: &str) -> SparseRow {
        let mut row = self.word.transform(document);
        row.extend(self.chars.transform(document));
        row.sort_unstable_by_key(|(index, _)| *index);
        row
    }
}

impl Vocabulary {
    fn fit(documents: &[&str], analyzer: Analyzer, offset: usize) -> Self {
        let mut document_frequency = HashMap::<String, usize>::new();
        for document in documents {
            let features: HashSet<_> = analyzer.features(document).into_iter().collect();
            for feature in features {
                *document_frequency.entry(feature).or_default() += 1;
            }
        }

        let mut features: Vec<_> = document_frequency
            .into_iter()
            .filter(|(_, frequency)| *frequency >= MIN_DF)
            .collect();
        features.sort_unstable_by(|left, right| left.0.cmp(&right.0));

        let documents = documents.len() as f32;
        let mut indices = HashMap::with_capacity(features.len());
        let mut idf = Vec::with_capacity(features.len());
        for (local_index, (feature, frequency)) in features.into_iter().enumerate() {
            indices.insert(feature, offset + local_index);
            idf.push(((1.0 + documents) / (1.0 + frequency as f32)).ln() + 1.0);
        }

        Self {
            analyzer,
            offset,
            indices,
            idf,
        }
    }

    fn len(&self) -> usize {
        self.idf.len()
    }

    fn transform(&self, document: &str) -> SparseRow {
        let mut counts = HashMap::<usize, usize>::new();
        for feature in self.analyzer.features(document) {
            if let Some(&index) = self.indices.get(&feature) {
                *counts.entry(index).or_default() += 1;
            }
        }

        let mut row: SparseRow = counts
            .into_iter()
            .map(|(index, count)| {
                let tf = 1.0 + (count as f32).ln();
                (index, tf * self.idf[index - self.offset])
            })
            .collect();
        let norm = row
            .iter()
            .map(|(_, value)| value * value)
            .sum::<f32>()
            .sqrt();
        if norm > 0.0 {
            for (_, value) in &mut row {
                *value /= norm;
            }
        }
        row
    }
}

impl Analyzer {
    fn features(self, document: &str) -> Vec<String> {
        match self {
            Self::Word => word_ngrams(document),
            Self::CharWb => char_wb_ngrams(document),
        }
    }
}

fn word_ngrams(document: &str) -> Vec<String> {
    let words: Vec<_> = document
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .collect();
    let mut features = Vec::with_capacity(words.len() * 2);
    for (index, word) in words.iter().enumerate() {
        features.push((*word).to_string());
        if let Some(next) = words.get(index + 1) {
            features.push(format!("{word} {next}"));
        }
    }
    features
}

fn char_wb_ngrams(document: &str) -> Vec<String> {
    let mut features = Vec::new();
    for word in document.split_whitespace() {
        let padded = format!(" {word} ");
        for size in 3..=5 {
            if padded.len() <= size {
                features.push(padded.clone());
            } else {
                for start in 0..=padded.len() - size {
                    features.push(padded[start..start + size].to_string());
                }
            }
        }
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tfidf_filters_singletons_and_normalises_each_analyzer() {
        let tfidf = Tfidf::fit(&["coffee shop", "coffee beans", "groceries"]).unwrap();
        let row = tfidf.transform("coffee shop");
        let word_norm: f32 = row
            .iter()
            .filter(|(index, _)| *index < tfidf.word.len())
            .map(|(_, value)| value * value)
            .sum();
        let char_norm: f32 = row
            .iter()
            .filter(|(index, _)| *index >= tfidf.word.len())
            .map(|(_, value)| value * value)
            .sum();

        assert!((word_norm - 1.0).abs() < 1e-5);
        assert!((char_norm - 1.0).abs() < 1e-5);
    }
}
