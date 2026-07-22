use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;

const MAX_DIFF_BYTES: usize = 120_000;
const MAX_COMMENTS: usize = 15;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-6";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.6-sol";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Provider {
    Anthropic,
    OpenAiResponses,
    OpenAiChat,
    Webhook,
}

impl Provider {
    fn from_env() -> Result<Self> {
        match env::var("REVIEW_PROVIDER")
            .unwrap_or_else(|_| "anthropic".to_string())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "anthropic" | "claude" => Ok(Self::Anthropic),
            "openai" | "openai-responses" | "responses" => Ok(Self::OpenAiResponses),
            "openai-chat" | "openai-compatible" | "chat-completions" => Ok(Self::OpenAiChat),
            "webhook" | "custom" => Ok(Self::Webhook),
            value => bail!(
                "unsupported REVIEW_PROVIDER '{value}'; expected anthropic, openai-responses, \
openai-compatible, or webhook"
            ),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiChat => "openai-compatible",
            Self::Webhook => "webhook",
        }
    }

    fn model(self) -> Result<String> {
        if let Some(model) = non_empty_env("REVIEW_MODEL") {
            return Ok(model);
        }
        match self {
            Self::Anthropic => Ok(DEFAULT_ANTHROPIC_MODEL.to_string()),
            Self::OpenAiResponses => Ok(DEFAULT_OPENAI_MODEL.to_string()),
            Self::OpenAiChat => bail!("REVIEW_MODEL is required for openai-compatible providers"),
            Self::Webhook => Ok(String::new()),
        }
    }

    fn api_key(self) -> Result<Option<String>> {
        let generic = non_empty_env("REVIEW_API_KEY");
        match self {
            Self::Anthropic => Ok(Some(
                generic
                    .or_else(|| non_empty_env("ANTHROPIC_API_KEY"))
                    .context("ANTHROPIC_API_KEY or REVIEW_API_KEY not set")?,
            )),
            Self::OpenAiResponses => Ok(Some(
                generic
                    .or_else(|| non_empty_env("OPENAI_API_KEY"))
                    .context("OPENAI_API_KEY or REVIEW_API_KEY not set")?,
            )),
            Self::OpenAiChat => Ok(generic.or_else(|| non_empty_env("OPENAI_API_KEY"))),
            Self::Webhook => Ok(generic),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Finding {
    path: String,
    line: u64,
    severity: String,
    comment: String,
}

#[derive(Debug, Deserialize)]
struct ReviewOutput {
    summary: String,
    #[serde(default)]
    findings: Vec<Finding>,
}

fn main() -> Result<()> {
    let github_token = required_env("GITHUB_TOKEN")?;
    let repo = required_env("GITHUB_REPOSITORY")?;
    let provider = Provider::from_env()?;
    let api_key = provider.api_key()?;
    let model = provider.model()?;

    let pr_number = pr_number()?;
    eprintln!(
        "Reviewing {repo}#{pr_number} with {}/{}",
        provider.name(),
        model
    );

    let diff = fetch_diff(&github_token, &repo, pr_number)?;
    if diff.trim().is_empty() {
        eprintln!("Empty diff, nothing to review.");
        return Ok(());
    }
    let diff = truncate_utf8(&diff, MAX_DIFF_BYTES);
    let commentable = commentable_lines(&diff);

    let review = run_review(provider, api_key.as_deref(), &model, &repo, &diff)?;
    post_review(&github_token, &repo, pr_number, review, &commentable)?;
    Ok(())
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_env(name: &str) -> Result<String> {
    non_empty_env(name).with_context(|| format!("{name} not set"))
}

/// PR number from the Actions event payload, with PR_NUMBER as a fallback
/// so the binary can also be run by hand.
fn pr_number() -> Result<u64> {
    if let Ok(n) = env::var("PR_NUMBER") {
        return n.parse().context("PR_NUMBER is not a number");
    }
    let path = env::var("GITHUB_EVENT_PATH").context("GITHUB_EVENT_PATH not set")?;
    let event: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    event["pull_request"]["number"]
        .as_u64()
        .or_else(|| event["issue"]["number"].as_u64())
        .context("could not find a PR number in the event payload")
}

fn fetch_diff(token: &str, repo: &str, pr: u64) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/pulls/{pr}");
    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github.v3.diff")
        .set("User-Agent", "pr-reviewer")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .context("fetching PR diff failed")?;
    Ok(resp.into_string()?)
}

/// Parse the unified diff and record, per file, which new-side line numbers
/// GitHub will accept a review comment on (added and context lines inside hunks).
fn commentable_lines(diff: &str) -> BTreeMap<String, BTreeSet<u64>> {
    let mut map: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut current_file: Option<String> = None;
    let mut new_line: u64 = 0;
    let mut remaining_new_lines: u64 = 0;

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            current_file = None;
            remaining_new_lines = 0;
        } else if let Some(rest) = line.strip_prefix("+++ b/") {
            current_file = Some(rest.to_string());
            remaining_new_lines = 0;
        } else if line.starts_with("+++ ") {
            current_file = None; // deleted file (+++ /dev/null)
            remaining_new_lines = 0;
        } else if line.starts_with("@@") {
            if let Some((start, count)) = parse_new_hunk_range(line) {
                new_line = start;
                remaining_new_lines = count;
            } else {
                remaining_new_lines = 0;
            }
        } else if remaining_new_lines > 0 {
            let Some(file) = &current_file else {
                continue;
            };
            if line.starts_with('+') || line.starts_with(' ') {
                map.entry(file.clone()).or_default().insert(new_line);
                new_line += 1;
                remaining_new_lines -= 1;
            } else if line.starts_with('-') {
                // old side only; new counter unchanged
            }
        }
    }
    map
}

fn parse_new_hunk_range(header: &str) -> Option<(u64, u64)> {
    let range = header
        .split_whitespace()
        .find(|part| part.starts_with('+'))?;
    let range = range.strip_prefix('+')?;
    let mut parts = range.splitn(2, ',');
    let start = parts.next()?.parse().ok()?;
    let count = parts.next().map(str::parse).transpose().ok()?.unwrap_or(1);
    Some((start, count))
}

fn review_system_prompt() -> &'static str {
    "You are a precise code reviewer. You receive a unified diff of a pull request. \
Report only genuine problems: bugs, logic errors, security issues, race conditions, resource leaks, \
API misuse, and broken edge cases. Do not comment on style, formatting, or naming unless it causes a bug. \
Line numbers must refer to the NEW file (the + side of the diff). \
Respond with ONLY a JSON object, no markdown fences, in this shape: \
{\"summary\": \"one-paragraph review summary\", \"findings\": [{\"path\": \"src/foo.rs\", \"line\": 42, \
\"severity\": \"high|medium|low\", \"comment\": \"what is wrong and how to fix it\"}]} \
If the change looks correct, return an empty findings array."
}

fn run_review(
    provider: Provider,
    api_key: Option<&str>,
    model: &str,
    repo: &str,
    diff: &str,
) -> Result<ReviewOutput> {
    match provider {
        Provider::Anthropic => run_anthropic_review(
            api_key.context("Anthropic API key missing")?,
            model,
            repo,
            diff,
        ),
        Provider::OpenAiResponses => run_openai_review(
            api_key.context("OpenAI API key missing")?,
            model,
            repo,
            diff,
        ),
        Provider::OpenAiChat => run_openai_chat_review(api_key, model, repo, diff),
        Provider::Webhook => run_webhook_review(api_key, model, repo, diff),
    }
}

fn run_anthropic_review(
    api_key: &str,
    model: &str,
    repo: &str,
    diff: &str,
) -> Result<ReviewOutput> {
    let user = format!("Repository: {repo}\n\nUnified diff:\n\n{diff}");

    let body = json!({
        "model": model,
        "max_tokens": 4000,
        "system": review_system_prompt(),
        "messages": [{ "role": "user", "content": user }],
    });

    let resp = ureq::post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", api_key)
        .set("anthropic-version", ANTHROPIC_VERSION)
        .set("content-type", "application/json")
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => anyhow::anyhow!(
                "Anthropic API returned {code}: {}",
                r.into_string().unwrap_or_default()
            ),
            other => anyhow::anyhow!("Anthropic API request failed: {other}"),
        })?;

    let v: Value = resp.into_json()?;
    let text = v["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .context("no text block in Anthropic response")?;

    parse_review_json(text)
}

fn run_openai_review(api_key: &str, model: &str, repo: &str, diff: &str) -> Result<ReviewOutput> {
    let input = format!("Repository: {repo}\n\nUnified diff:\n\n{diff}");
    let body = json!({
        "model": model,
        "instructions": review_system_prompt(),
        "input": input,
        "max_output_tokens": 4000,
        "store": false,
        "text": {
            "format": {
                "type": "json_schema",
                "name": "pr_review",
                "strict": true,
                "schema": review_json_schema()
            }
        }
    });

    let base_url = review_base_url();
    let url = format!("{}/responses", base_url.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => anyhow::anyhow!(
                "OpenAI API returned {code}: {}",
                r.into_string().unwrap_or_default()
            ),
            other => anyhow::anyhow!("OpenAI API request failed: {other}"),
        })?;

    let v: Value = resp.into_json()?;
    parse_openai_response(&v)
}

fn run_openai_chat_review(
    api_key: Option<&str>,
    model: &str,
    repo: &str,
    diff: &str,
) -> Result<ReviewOutput> {
    let user = format!("Repository: {repo}\n\nUnified diff:\n\n{diff}");
    // Keep this request deliberately minimal. Most OpenAI-compatible services
    // accept messages/model, while vendor-specific output and token controls vary.
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": review_system_prompt() },
            { "role": "user", "content": user }
        ]
    });
    let url = format!(
        "{}/chat/completions",
        review_base_url().trim_end_matches('/')
    );
    let response = send_generic_request(&url, "OpenAI-compatible API", api_key, body)?;
    parse_openai_chat_response(&response)
}

fn parse_openai_chat_response(v: &Value) -> Result<ReviewOutput> {
    let content = &v["choices"][0]["message"]["content"];
    let text = content.as_str().or_else(|| {
        content
            .as_array()
            .and_then(|parts| parts.iter().find_map(|part| part["text"].as_str()))
    });
    parse_review_json(text.context("no message content in OpenAI-compatible response")?)
}

fn run_webhook_review(
    api_key: Option<&str>,
    model: &str,
    repo: &str,
    diff: &str,
) -> Result<ReviewOutput> {
    let url = required_env("REVIEW_ENDPOINT")?;
    let body = json!({
        "task": "pull_request_review",
        "model": if model.is_empty() { Value::Null } else { json!(model) },
        "system": review_system_prompt(),
        "repository": repo,
        "diff": diff,
        "output_schema": review_json_schema()
    });
    let response = send_generic_request(&url, "review webhook", api_key, body)?;
    parse_generic_review_response(&response)
}

fn review_base_url() -> String {
    non_empty_env("REVIEW_BASE_URL")
        .or_else(|| non_empty_env("OPENAI_BASE_URL"))
        .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
}

fn send_generic_request(
    url: &str,
    label: &str,
    api_key: Option<&str>,
    body: Value,
) -> Result<Value> {
    let mut request = ureq::post(url).set("Content-Type", "application/json");
    if let Some(key) = api_key {
        let header =
            non_empty_env("REVIEW_AUTH_HEADER").unwrap_or_else(|| "Authorization".to_string());
        let configured_scheme =
            non_empty_env("REVIEW_AUTH_SCHEME").unwrap_or_else(|| "Bearer".to_string());
        let scheme = if configured_scheme.eq_ignore_ascii_case("none") {
            ""
        } else {
            configured_scheme.as_str()
        };
        let value = if scheme.is_empty() {
            key.to_string()
        } else {
            format!("{scheme} {key}")
        };
        request = request.set(&header, &value);
    }

    let response = request.send_json(body).map_err(|error| match error {
        ureq::Error::Status(code, response) => anyhow::anyhow!(
            "{label} returned {code}: {}",
            response.into_string().unwrap_or_default()
        ),
        other => anyhow::anyhow!("{label} request failed: {other}"),
    })?;
    Ok(response.into_json()?)
}

fn parse_generic_review_response(v: &Value) -> Result<ReviewOutput> {
    if let Some(pointer) = non_empty_env("REVIEW_RESPONSE_JSON_POINTER") {
        let selected = v
            .pointer(&pointer)
            .with_context(|| format!("REVIEW_RESPONSE_JSON_POINTER '{pointer}' did not match"))?;
        return parse_review_value(selected);
    }
    parse_review_value(v)
}

fn parse_review_value(value: &Value) -> Result<ReviewOutput> {
    if let Some(text) = value.as_str() {
        return parse_review_json(text);
    }
    if value.get("summary").is_some() && value.get("findings").is_some() {
        return serde_json::from_value(value.clone()).context("invalid normalized review response");
    }
    for key in ["review", "output", "result", "data", "text", "content"] {
        if let Some(nested) = value.get(key) {
            if let Ok(review) = parse_review_value(nested) {
                return Ok(review);
            }
        }
    }
    bail!(
        "webhook response did not contain a review; return summary/findings directly or configure \
REVIEW_RESPONSE_JSON_POINTER"
    )
}

fn parse_openai_response(v: &Value) -> Result<ReviewOutput> {
    if v["status"] != "completed" {
        bail!(
            "OpenAI response was not completed (status: {}): {}",
            v["status"],
            v["error"]
        );
    }
    let text = v["output"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["type"] == "message"))
        .and_then(|message| message["content"].as_array())
        .and_then(|content| content.iter().find(|item| item["type"] == "output_text"))
        .and_then(|item| item["text"].as_str())
        .context("no output_text block in OpenAI response")?;

    parse_review_json(text)
}

fn review_json_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer", "minimum": 1 },
                        "severity": {
                            "type": "string",
                            "enum": ["high", "medium", "low"]
                        },
                        "comment": { "type": "string" }
                    },
                    "required": ["path", "line", "severity", "comment"]
                }
            }
        },
        "required": ["summary", "findings"]
    })
}

fn parse_review_json(text: &str) -> Result<ReviewOutput> {
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let parsed: ReviewOutput = serde_json::from_str(cleaned)
        .with_context(|| format!("model did not return valid JSON:\n{cleaned}"))?;
    Ok(parsed)
}

fn post_review(
    token: &str,
    repo: &str,
    pr: u64,
    review: ReviewOutput,
    commentable: &BTreeMap<String, BTreeSet<u64>>,
) -> Result<()> {
    let mut comments = Vec::new();
    let mut orphaned = Vec::new();
    let mut fallback_findings = Vec::new();

    for f in review.findings {
        let rendered = format!("- `{}:{}` [{}] {}", f.path, f.line, f.severity, f.comment);
        fallback_findings.push(rendered.clone());
        let valid = commentable
            .get(&f.path)
            .map(|lines| lines.contains(&f.line))
            .unwrap_or(false);
        if valid && comments.len() < MAX_COMMENTS {
            comments.push(json!({
                "path": f.path,
                "line": f.line,
                "side": "RIGHT",
                "body": format!("**[{}]** {}", f.severity, f.comment),
            }));
        } else {
            orphaned.push(rendered);
        }
    }

    let mut body = format!("## AI review\n\n{}", review.summary);
    if !orphaned.is_empty() {
        body.push_str("\n\n**Findings outside the diff:**\n");
        body.push_str(&orphaned.join("\n"));
    }

    let url = format!("https://api.github.com/repos/{repo}/pulls/{pr}/reviews");
    let payload = json!({ "event": "COMMENT", "body": body, "comments": comments });

    let result = ureq::post(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "pr-reviewer")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .send_json(payload);

    match result {
        Ok(_) => {
            eprintln!("Posted review with {} line comments.", comments.len());
            Ok(())
        }
        Err(ureq::Error::Status(422, r)) => {
            // Some line anchor was rejected anyway; fall back to summary-only.
            eprintln!(
                "422 posting line comments, falling back to summary only: {}",
                r.into_string().unwrap_or_default()
            );
            let mut fallback_body = body;
            if !fallback_findings.is_empty() {
                fallback_body.push_str("\n\n**Line findings:**\n");
                fallback_body.push_str(&fallback_findings.join("\n"));
            }
            let fallback = json!({ "event": "COMMENT", "body": fallback_body });
            ureq::post(&url)
                .set("Authorization", &format!("Bearer {token}"))
                .set("Accept", "application/vnd.github+json")
                .set("User-Agent", "pr-reviewer")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .send_json(fallback)
                .context("fallback summary review also failed")?;
            Ok(())
        }
        Err(e) => bail!("posting review failed: {e}"),
    }
}

fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n[diff truncated at {max} bytes]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunk_lines() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@ fn foo() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
 let d = 5;
";
        let map = commentable_lines(diff);
        let lines = map.get("src/lib.rs").unwrap();
        // context 10, added 11, added 12, context 13
        assert_eq!(
            lines.iter().copied().collect::<Vec<_>>(),
            vec![10, 11, 12, 13]
        );
    }

    #[test]
    fn ignores_diff_metadata_outside_hunks() {
        let diff = "\
diff --git a/one.rs b/one.rs
--- a/one.rs
+++ b/one.rs
@@ -5 +5 @@
+changed
\\ No newline at end of file
diff --git a/two.rs b/two.rs
index 123..456 100644
--- a/two.rs
+++ b/two.rs
@@ -10,0 +11,2 @@
+first
+second
";

        let map = commentable_lines(diff);
        assert_eq!(map["one.rs"].iter().copied().collect::<Vec<_>>(), vec![5]);
        assert_eq!(
            map["two.rs"].iter().copied().collect::<Vec<_>>(),
            vec![11, 12]
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        let s = "héllo".repeat(100);
        let t = truncate_utf8(&s, 7);
        assert!(t.starts_with("héllo"));
    }

    #[test]
    fn parses_openai_structured_output() {
        let response = json!({
            "status": "completed",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "{\"summary\":\"Looks good\",\"findings\":[]}"
                }]
            }]
        });

        let review = parse_openai_response(&response).unwrap();
        assert_eq!(review.summary, "Looks good");
        assert!(review.findings.is_empty());
    }

    #[test]
    fn rejects_incomplete_openai_response() {
        let response = json!({
            "status": "incomplete",
            "error": null,
            "output": []
        });

        let error = parse_openai_response(&response).unwrap_err();
        assert!(error.to_string().contains("not completed"));
    }

    #[test]
    fn openai_schema_is_strict() {
        let schema = review_json_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["findings"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn parses_openai_compatible_chat_output() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": "{\"summary\":\"Compatible\",\"findings\":[]}"
                }
            }]
        });

        let review = parse_openai_chat_response(&response).unwrap();
        assert_eq!(review.summary, "Compatible");
    }

    #[test]
    fn parses_wrapped_webhook_output() {
        let response = json!({
            "data": {
                "output": {
                    "summary": "Webhook",
                    "findings": []
                }
            }
        });

        let review = parse_review_value(&response).unwrap();
        assert_eq!(review.summary, "Webhook");
    }
}
