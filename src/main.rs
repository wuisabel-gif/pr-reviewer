mod benchmark;
mod github;
mod model;

use anyhow::{bail, Result};
use model::{Provider, ReviewRequest};
use std::env;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let provider = Provider::from_env()?;
    let api_key = provider.api_key()?;
    let model = provider.model()?;
    let (passes, threshold) = model::voting_config()?;

    if args.get(1).map(String::as_str) == Some("--benchmark") {
        let path = args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("usage: pr-reviewer --benchmark <suite.json>"))?;
        return benchmark::run(
            path,
            provider,
            api_key.as_deref(),
            &model,
            passes,
            threshold,
        );
    }
    if args.len() > 1 {
        bail!(
            "unknown argument '{}'; use --benchmark <suite.json>",
            args[1]
        );
    }

    let github_token = github::required_env("GITHUB_TOKEN")?;
    let repo = github::required_env("GITHUB_REPOSITORY")?;
    let pr_number = github::pr_number()?;

    eprintln!(
        "Reviewing {repo}#{pr_number} with {}/{} ({passes} pass(es), threshold {threshold})",
        provider.name(),
        model
    );

    let input = github::load_review_input(&github_token, &repo, pr_number)?;
    if input.diff.trim().is_empty() {
        eprintln!("Empty diff, nothing to review.");
        return Ok(());
    }

    let request = ReviewRequest {
        repo: &repo,
        diff: &input.diff,
        context: &input.context,
        rules: &input.rules,
    };
    let mut review = model::run_consensus(
        provider,
        api_key.as_deref(),
        &model,
        &request,
        passes,
        threshold,
    )?;

    let existing = match github::existing_fingerprints(&github_token, &repo, pr_number) {
        Ok(existing) => existing,
        Err(error) => {
            eprintln!("Could not load existing review fingerprints: {error:#}");
            Default::default()
        }
    };
    let before = review.findings.len();
    review
        .findings
        .retain(|finding| !existing.contains(&github::finding_fingerprint(finding)));
    let skipped = before - review.findings.len();
    if skipped > 0 {
        review.summary.push_str(&format!(
            " {skipped} finding(s) were omitted because they were already posted on this PR."
        ));
    }

    github::post_review(&github_token, &repo, pr_number, review, &input.commentable)?;
    Ok(())
}
