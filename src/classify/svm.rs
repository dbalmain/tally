use std::collections::HashMap;

use super::tfidf::SparseRow;

const EPOCHS: usize = 12;
const PA_C: f32 = 0.25;

pub(super) struct LinearSvm {
    classes: Vec<i64>,
    weights: Vec<Vec<f32>>,
    biases: Vec<f32>,
}

impl LinearSvm {
    pub(super) fn train(rows: &[SparseRow], labels: &[i64], feature_count: usize) -> Option<Self> {
        if rows.is_empty() || rows.len() != labels.len() || feature_count == 0 {
            return None;
        }

        let mut counts = HashMap::<i64, usize>::new();
        for &label in labels {
            *counts.entry(label).or_default() += 1;
        }
        let mut classes: Vec<_> = counts.keys().copied().collect();
        classes.sort_unstable();
        if classes.len() == 1 {
            return Some(Self {
                classes,
                weights: vec![vec![0.0; feature_count]],
                biases: vec![1.0],
            });
        }

        let mut weights = vec![vec![0.0; feature_count]; classes.len()];
        let mut totals = vec![vec![0.0; feature_count]; classes.len()];
        let mut last = vec![vec![0_u32; feature_count]; classes.len()];
        let mut biases = vec![0.0; classes.len()];
        let mut bias_totals = vec![0.0; classes.len()];
        let mut bias_last = vec![0_u32; classes.len()];
        let mut order: Vec<_> = (0..rows.len()).collect();
        let mut seed = 0x9e37_79b9_u32;
        let mut step = 0_u32;

        for _ in 0..EPOCHS {
            shuffle(&mut order, &mut seed);
            for &row_index in &order {
                step += 1;
                let row = &rows[row_index];
                let norm_sq = 1.0 + row.iter().map(|(_, value)| value * value).sum::<f32>();
                for (class_index, &class) in classes.iter().enumerate() {
                    let target = if labels[row_index] == class {
                        1.0
                    } else {
                        -1.0
                    };
                    let score = dot(&weights[class_index], row) + biases[class_index];
                    let loss = 1.0 - target * score;
                    if loss <= 0.0 {
                        continue;
                    }

                    let positives = counts[&class] as f32;
                    let class_weight = if target > 0.0 {
                        rows.len() as f32 / (2.0 * positives)
                    } else {
                        rows.len() as f32 / (2.0 * (rows.len() as f32 - positives))
                    };
                    let tau = (loss / norm_sq).min(PA_C * class_weight);
                    let delta = tau * target;
                    for &(feature, value) in row {
                        accumulate(
                            &mut totals[class_index][feature],
                            &mut last[class_index][feature],
                            weights[class_index][feature],
                            step,
                        );
                        weights[class_index][feature] += delta * value;
                    }
                    accumulate(
                        &mut bias_totals[class_index],
                        &mut bias_last[class_index],
                        biases[class_index],
                        step,
                    );
                    biases[class_index] += delta;
                }
            }
        }

        let end = step + 1;
        for class_index in 0..classes.len() {
            for feature in 0..feature_count {
                accumulate(
                    &mut totals[class_index][feature],
                    &mut last[class_index][feature],
                    weights[class_index][feature],
                    end,
                );
                weights[class_index][feature] = totals[class_index][feature] / step as f32;
            }
            accumulate(
                &mut bias_totals[class_index],
                &mut bias_last[class_index],
                biases[class_index],
                end,
            );
            biases[class_index] = bias_totals[class_index] / step as f32;
        }

        Some(Self {
            classes,
            weights,
            biases,
        })
    }

    pub(super) fn predict(&self, row: &SparseRow) -> (i64, f32) {
        let mut scores: Vec<_> = self
            .classes
            .iter()
            .enumerate()
            .map(|(index, &class)| (class, dot(&self.weights[index], row) + self.biases[index]))
            .collect();
        scores.sort_unstable_by(|left, right| right.1.total_cmp(&left.1));
        let margin = scores
            .get(1)
            .map_or(scores[0].1.abs(), |runner_up| scores[0].1 - runner_up.1);
        (scores[0].0, margin)
    }
}

fn dot(weights: &[f32], row: &SparseRow) -> f32 {
    row.iter()
        .map(|&(feature, value)| weights[feature] * value)
        .sum()
}

fn accumulate(total: &mut f32, last: &mut u32, weight: f32, step: u32) {
    *total += (step - *last) as f32 * weight;
    *last = step;
}

fn shuffle(values: &mut [usize], seed: &mut u32) {
    for index in (1..values.len()).rev() {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        values.swap(index, *seed as usize % (index + 1));
    }
}
