use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;

use crate::search::ParsedQuery;
use crate::{
    CategorySource, ConfirmedCategoryExample, Error, Result, Transaction, TransactionStore,
};

const DEFAULT_EMBED_MODEL: &str = "embeddinggemma";
const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
const DEFAULT_TIER1_MIN_SIMILARITY: f32 = 0.80;

/// Counts produced by one local classification run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClassifyReport {
    pub tier0: usize,
    pub tier1: usize,
    pub unclassified: usize,
}

#[derive(Debug, Deserialize)]
struct CollectionConfig {
    #[serde(default)]
    classify: ClassifyConfig,
}

#[derive(Debug, Deserialize)]
struct ClassifyConfig {
    #[serde(default = "default_embed_model")]
    embed_model: String,
    #[serde(default = "default_ollama_url")]
    ollama_url: String,
    #[serde(default = "default_tier1_min_similarity")]
    tier1_min_similarity: f32,
}

impl Default for ClassifyConfig {
    fn default() -> Self {
        Self {
            embed_model: default_embed_model(),
            ollama_url: default_ollama_url(),
            tier1_min_similarity: default_tier1_min_similarity(),
        }
    }
}

fn default_embed_model() -> String {
    DEFAULT_EMBED_MODEL.to_string()
}

fn default_ollama_url() -> String {
    DEFAULT_OLLAMA_URL.to_string()
}

fn default_tier1_min_similarity() -> f32 {
    DEFAULT_TIER1_MIN_SIMILARITY
}

fn load_config(collection_root: &Path) -> Result<ClassifyConfig> {
    let path = collection_root.join("collection.toml");
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str::<CollectionConfig>(&contents)?.classify),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ClassifyConfig::default()),
        Err(error) => Err(error.into()),
    }
}

/// Normalize a bank description into a merchant-like matching key.
pub fn normalise(description: &str) -> String {
    static CARD_DETAILS: OnceLock<Regex> = OnceLock::new();
    static REFERENCE: OnceLock<Regex> = OnceLock::new();
    static NOISE: OnceLock<Regex> = OnceLock::new();
    static DIGITS: OnceLock<Regex> = OnceLock::new();
    static PUNCTUATION: OnceLock<Regex> = OnceLock::new();
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();

    let mut text = description.to_lowercase();
    text = CARD_DETAILS
        .get_or_init(|| {
            Regex::new(
                r"\b(?:(?:visa|mastercard|debit|credit)\s+)?card\s*(?:ending\s+(?:in\s+)?)?(?:x+|\*+)?\s*\d{2,}\b",
            )
            .unwrap()
        })
        .replace_all(&text, " ")
        .into_owned();
    text = REFERENCE
        .get_or_init(|| {
            Regex::new(
                r"\b(?:ref(?:erence)?|txn|transaction)\s*(?:no|number)?\s*[:#-]?\s*[a-z0-9*-]*\d[a-z0-9*-]*\b",
            )
            .unwrap()
        })
        .replace_all(&text, " ")
        .into_owned();
    text = NOISE
        .get_or_init(|| {
            Regex::new(
                r"\b(?:visa|mastercard|debit|credit|card|eftpos|pos|purchase|ref|reference|txn|transaction)\b",
            )
            .unwrap()
        })
        .replace_all(&text, " ")
        .into_owned();
    text = DIGITS
        .get_or_init(|| Regex::new(r"\d+").unwrap())
        .replace_all(&text, " ")
        .into_owned();
    text = PUNCTUATION
        .get_or_init(|| Regex::new(r"[^a-z\s]+").unwrap())
        .replace_all(&text, " ")
        .into_owned();
    WHITESPACE
        .get_or_init(|| Regex::new(r"\s+").unwrap())
        .replace_all(text.trim(), " ")
        .into_owned()
}

/// Return cosine similarity, or zero for empty, zero-length, or mismatched vectors.
pub fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }

    let (dot, left_norm, right_norm) = left.iter().zip(right).fold(
        (0.0_f64, 0.0_f64, 0.0_f64),
        |(dot, left_norm, right_norm), (&left, &right)| {
            let left = f64::from(left);
            let right = f64::from(right);
            (
                dot + left * right,
                left_norm + left * left,
                right_norm + right * right,
            )
        },
    );
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }

    (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0) as f32
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

struct OllamaClient<'a> {
    url: &'a str,
    model: &'a str,
}

impl OllamaClient<'_> {
    fn embed(&self, input: &str) -> Result<Vec<f32>> {
        let url = format!("{}/api/embed", self.url.trim_end_matches('/'));
        let response = ureq::post(&url)
            .send_json(serde_json::json!({
                "model": self.model,
                "input": input,
            }))
            .map_err(|error| Error::Http(format!("Ollama embed request failed: {error}")))?;
        let response: EmbedResponse = response
            .into_json()
            .map_err(|error| Error::Http(format!("invalid Ollama embed response: {error}")))?;
        let embedding = response.embeddings.into_iter().next().ok_or_else(|| {
            Error::InvalidEmbedding("Ollama returned no embedding vectors".to_string())
        })?;
        if embedding.is_empty() {
            return Err(Error::InvalidEmbedding(
                "Ollama returned an empty vector".to_string(),
            ));
        }
        Ok(embedding)
    }
}

fn tier0_categories(examples: &[ConfirmedCategoryExample]) -> HashMap<String, i64> {
    let mut counts: HashMap<String, HashMap<(String, i64), usize>> = HashMap::new();
    for example in examples {
        let norm = normalise(&example.description);
        if norm.is_empty() {
            continue;
        }
        *counts
            .entry(norm)
            .or_default()
            .entry((example.category_path.clone(), example.category_id))
            .or_default() += 1;
    }

    counts
        .into_iter()
        .map(|(norm, categories)| {
            let mut categories: Vec<_> = categories.into_iter().collect();
            categories.sort_by(|((path_a, id_a), count_a), ((path_b, id_b), count_b)| {
                count_b
                    .cmp(count_a)
                    .then_with(|| path_a.cmp(path_b))
                    .then_with(|| id_a.cmp(id_b))
            });
            (norm, categories[0].0.1)
        })
        .collect()
}

fn cached_or_embed(
    store: &mut TransactionStore,
    client: &OllamaClient<'_>,
    norm: &str,
) -> Result<Vec<f32>> {
    if let Some(embedding) = store.get_cached_embedding(norm, client.model)? {
        return Ok(embedding);
    }

    let embedding = client.embed(norm)?;
    store.put_cached_embedding(norm, client.model, &embedding)?;
    Ok(embedding)
}

/// Classify all currently uncategorized, non-transfer transactions in a collection.
pub fn classify(store: &mut TransactionStore, collection_root: &Path) -> Result<ClassifyReport> {
    let config = load_config(collection_root)?;
    let examples = store.get_confirmed_examples()?;
    let tier0 = tier0_categories(&examples);
    let transactions = store.get_uncategorised_transactions(&ParsedQuery::empty(), None)?;
    let mut report = ClassifyReport::default();
    let mut remaining: Vec<(Transaction, String)> = Vec::new();

    for transaction in transactions {
        let norm = normalise(&transaction.description);
        if let Some(&category_id) = tier0.get(&norm) {
            store.set_category(
                transaction.id,
                category_id,
                CategorySource::Ai,
                false,
                Some(1.0),
            )?;
            report.tier0 += 1;
        } else {
            remaining.push((transaction, norm));
        }
    }

    if remaining.is_empty() || tier0.is_empty() {
        report.unclassified = remaining.len();
        return Ok(report);
    }

    let client = OllamaClient {
        url: &config.ollama_url,
        model: &config.embed_model,
    };
    let mut seen_norms = HashSet::new();
    let mut embedded_examples = Vec::new();
    for example in &examples {
        let norm = normalise(&example.description);
        if norm.is_empty() || !seen_norms.insert(norm.clone()) {
            continue;
        }
        let category_id = tier0[&norm];
        let embedding = cached_or_embed(store, &client, &norm)?;
        embedded_examples.push((category_id, embedding));
    }

    for (transaction, norm) in remaining {
        if norm.is_empty() {
            report.unclassified += 1;
            continue;
        }

        let embedding = cached_or_embed(store, &client, &norm)?;
        let best = embedded_examples
            .iter()
            .map(|(category_id, example_embedding)| {
                (*category_id, cosine(&embedding, example_embedding))
            })
            .max_by(|left, right| left.1.total_cmp(&right.1));

        if let Some((category_id, similarity)) =
            best.filter(|(_, similarity)| *similarity >= config.tier1_min_similarity)
        {
            store.set_category(
                transaction.id,
                category_id,
                CategorySource::Ai,
                false,
                Some(f64::from(similarity)),
            )?;
            report.tier1 += 1;
        } else {
            report.unclassified += 1;
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(
        description: &str,
        category_id: i64,
        category_path: &str,
    ) -> ConfirmedCategoryExample {
        ConfirmedCategoryExample {
            description: description.to_string(),
            category_id,
            category_path: category_path.to_string(),
        }
    }

    #[test]
    fn normalise_removes_reference_noise_and_preserves_descriptive_words() {
        assert_eq!(
            normalise("CARD PURCHASE COLES 0531 RICHMOND"),
            "coles richmond"
        );
        assert_eq!(normalise("COLES 1234 NEWTOWN"), "coles newtown");
        assert_eq!(normalise("CARD 1234 COLES REF: 987654"), "coles");
        assert_eq!(
            normalise("  The-Coffee.Shop!! Ref: 987654  "),
            "the coffee shop"
        );
    }

    #[test]
    fn cosine_handles_common_vector_cases() {
        assert!((cosine(&[1.0, 2.0], &[2.0, 4.0]) - 1.0).abs() < f32::EPSILON);
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn tier0_uses_the_majority_category_with_a_deterministic_tie_break() {
        let choices = tier0_categories(&[
            example("COLES 0531 RICHMOND", 2, "Food/Groceries"),
            example("COLES 1234 RICHMOND", 2, "Food/Groceries"),
            example("COLES 9999 RICHMOND", 3, "Shopping/General"),
            example("CAFE 111 SYDNEY", 5, "Food/Zed"),
            example("CAFE 222 SYDNEY", 4, "Food/Alpha"),
        ]);

        assert_eq!(choices.get("coles richmond"), Some(&2));
        assert_eq!(choices.get("cafe sydney"), Some(&4));
    }
}
