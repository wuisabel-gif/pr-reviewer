use crate::model::{self, Provider, ReviewRequest};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;

#[derive(Deserialize)]
struct BenchmarkSuite {
    cases: Vec<BenchmarkCase>,
}

#[derive(Deserialize)]
struct BenchmarkCase {
    name: String,
    #[serde(default = "default_repository")]
    repository: String,
    diff: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    rules: String,
    #[serde(default)]
    expected: Vec<ExpectedFinding>,
}

#[derive(Deserialize)]
struct ExpectedFinding {
    path: String,
    line: u64,
}

fn default_repository() -> String {
    "benchmark/repository".to_string()
}

pub fn run(
    path: &str,
    provider: Provider,
    api_key: Option<&str>,
    model_name: &str,
    passes: usize,
    threshold: usize,
) -> Result<()> {
    let suite: BenchmarkSuite = serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("reading benchmark suite {path}"))?,
    )
    .with_context(|| format!("parsing benchmark suite {path}"))?;

    let case_count = suite.cases.len();
    let mut total_expected = 0_usize;
    let mut total_predicted = 0_usize;
    let mut total_true_positive = 0_usize;

    for case in suite.cases {
        let request = ReviewRequest {
            repo: &case.repository,
            diff: &case.diff,
            context: &case.context,
            rules: &case.rules,
        };
        let review =
            model::run_consensus(provider, api_key, model_name, &request, passes, threshold)?;
        let expected: BTreeSet<(String, u64)> = case
            .expected
            .into_iter()
            .map(|finding| (finding.path, finding.line))
            .collect();
        let predicted: BTreeSet<(String, u64)> = review
            .findings
            .into_iter()
            .map(|finding| (finding.path, finding.line))
            .collect();
        let true_positive = expected.intersection(&predicted).count();
        total_expected += expected.len();
        total_predicted += predicted.len();
        total_true_positive += true_positive;
        eprintln!(
            "{}: expected {}, predicted {}, matched {}",
            case.name,
            expected.len(),
            predicted.len(),
            true_positive
        );
    }

    let precision = ratio(total_true_positive, total_predicted, total_expected == 0);
    let recall = ratio(total_true_positive, total_expected, total_predicted == 0);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "cases": case_count,
            "expected": total_expected,
            "predicted": total_predicted,
            "true_positives": total_true_positive,
            "precision": precision,
            "recall": recall
        }))?
    );
    Ok(())
}

fn ratio(numerator: usize, denominator: usize, empty_is_perfect: bool) -> f64 {
    if denominator == 0 {
        if empty_is_perfect {
            1.0
        } else {
            0.0
        }
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_handle_empty_sets() {
        assert_eq!(ratio(0, 0, true), 1.0);
        assert_eq!(ratio(0, 0, false), 0.0);
        assert_eq!(ratio(1, 2, false), 0.5);
    }
}
