use std::collections::HashMap;
use std::fs;

use chrono::NaiveDate;

use super::*;

fn date(value: &str) -> NaiveDate {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
}

fn example(norm: &str, amount_cents: i64, date_value: &str, category_id: i64) -> Example {
    Example {
        norm: norm.to_string(),
        amount_cents,
        date: date(date_value),
        category_id,
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
fn normalise_pipeline_stages_are_characterised() {
    for (input, expected) in [
        ("Coffee SHOP", "coffee shop"),
        ("Metro card ending in 1234", "metro"),
        ("Metro txn no AB12CD", "metro"),
        ("visa purchase metro eftpos", "metro"),
        ("metro 123 george", "metro george"),
        ("metro-cafe!", "metro cafe"),
        ("  metro   cafe  ", "metro cafe"),
    ] {
        assert_eq!(normalise(input), expected, "input: {input:?}");
    }
}

#[test]
fn temporal_cascade_is_strict_and_preserves_most_recent_label() {
    let classifier = train(&[
        example("utility", 1000, "2025-01-01", 1),
        example("utility", 2000, "2025-02-01", 2),
        example("utility", 1000, "2025-03-01", 2),
    ]);

    let exact = predict(
        &classifier,
        &Input {
            norm: "utility".into(),
            amount_cents: 1000,
            date: date("2025-04-01"),
        },
    )
    .unwrap();
    assert_eq!(exact.category_id, 2);
    assert_eq!(exact.source, PredictionSource::Exact);
    assert_eq!(exact.confidence, EXACT_CONFIDENCE);

    let recurring = predict(
        &classifier,
        &Input {
            norm: "utility".into(),
            amount_cents: 3000,
            date: date("2025-03-01"),
        },
    )
    .unwrap();
    assert_eq!(recurring.category_id, 2);
    assert_eq!(recurring.source, PredictionSource::Recurring);
    assert_eq!(recurring.confidence, RECURRING_LOW_CONFIDENCE);
}

#[test]
fn unambiguous_recurring_history_gets_high_confidence() {
    let classifier = train(&[
        example("internet", 5000, "2025-01-01", 4),
        example("internet", 5500, "2025-02-01", 4),
    ]);
    let prediction = predict(
        &classifier,
        &Input {
            norm: "internet".into(),
            amount_cents: 6000,
            date: date("2025-03-01"),
        },
    )
    .unwrap();

    assert_eq!(prediction.category_id, 4);
    assert_eq!(prediction.confidence, RECURRING_HIGH_CONFIDENCE);
}

#[test]
fn model_classifies_novel_descriptions() {
    let classifier = train(&[
        example("coffee shop", 100, "2025-01-01", 1),
        example("coffee beans", 100, "2025-01-02", 1),
        example("grocery market", 100, "2025-01-01", 2),
        example("grocery store", 100, "2025-01-02", 2),
    ]);
    let prediction = predict(
        &classifier,
        &Input {
            norm: "coffee market".into(),
            amount_cents: 100,
            date: date("2024-01-01"),
        },
    )
    .unwrap();

    assert_eq!(prediction.category_id, 1);
    assert_eq!(prediction.source, PredictionSource::Model);
}

#[test]
fn model_abstains_with_single_category() {
    // Every confirmed example is in one category, so the model has nothing to
    // discriminate. A novel biller (no history match) must get no suggestion
    // rather than the lone category at a meaningless fixed confidence.
    let classifier = train(&[
        example("coffee shop", 100, "2025-01-01", 1),
        example("coffee beans", 100, "2025-01-02", 1),
        example("grocery market", 100, "2025-01-03", 1),
    ]);

    assert!(
        predict(
            &classifier,
            &Input {
                norm: "tea house".into(),
                amount_cents: 100,
                date: date("2025-02-01"),
            },
        )
        .is_none()
    );

    // History still tags a known biller even though the model abstains.
    let known = predict(
        &classifier,
        &Input {
            norm: "coffee shop".into(),
            amount_cents: 100,
            date: date("2025-02-01"),
        },
    )
    .unwrap();
    assert_eq!(known.category_id, 1);
}

#[test]
#[ignore = "requires TALLY_EVAL_CSV"]
fn classify_eval_csv_accuracy() {
    let Some(path) = std::env::var_os("TALLY_EVAL_CSV") else {
        return;
    };
    let contents = fs::read_to_string(path).unwrap();
    let mut lines = contents.lines();
    let headers = parse_csv_row(lines.next().unwrap());
    let column = |name: &str| headers.iter().position(|header| header == name).unwrap();
    let date_column = column("date");
    let description_column = column("description");
    let amount_column = column("amount_cents");
    let category_column = column("category");
    let split_column = column("split");
    let mut category_ids = HashMap::<String, i64>::new();
    let mut examples = Vec::new();
    let mut test = Vec::new();

    for line in lines.filter(|line| !line.trim().is_empty()) {
        let row = parse_csv_row(line);
        let next_id = category_ids.len() as i64;
        let category_id = *category_ids
            .entry(row[category_column].clone())
            .or_insert(next_id);
        let input = Input {
            norm: normalise(&row[description_column]),
            amount_cents: row[amount_column].parse().unwrap(),
            date: date(&row[date_column]),
        };
        match row[split_column].trim() {
            "train" => examples.push(Example {
                norm: input.norm,
                amount_cents: input.amount_cents,
                date: input.date,
                category_id,
            }),
            "test" => test.push((input, category_id)),
            split => panic!("unexpected split {split:?}"),
        }
    }

    let classifier = train(&examples);
    let mut correct = 0;
    let mut tiers = [0_usize; 4];
    for (input, expected) in &test {
        match predict(&classifier, input) {
            Some(prediction) => {
                correct += usize::from(prediction.category_id == *expected);
                tiers[match prediction.source {
                    PredictionSource::Exact => 0,
                    PredictionSource::Recurring => 1,
                    PredictionSource::Model => 2,
                }] += 1;
            }
            None => tiers[3] += 1,
        }
    }
    let accuracy = correct as f64 / test.len() as f64;
    println!(
        "classify accuracy: {:.2}% ({correct}/{}) exact={} recurring={} model={} unclassified={}",
        accuracy * 100.0,
        test.len(),
        tiers[0],
        tiers[1],
        tiers[2],
        tiers[3]
    );
    assert!(
        accuracy >= 0.85,
        "accuracy {:.2}% is below 85%",
        accuracy * 100.0
    );
}

fn parse_csv_row(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.trim_end_matches('\r').chars().peekable();
    let mut quoted = false;
    while let Some(character) = chars.next() {
        match character {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(character),
        }
    }
    fields.push(field);
    fields
}
