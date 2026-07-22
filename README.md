# pr-reviewer

> **Change the agent, not the reviewer.**

An agent-agnostic AI code reviewer, written in Rust and packaged as a reusable GitHub Action. It supports native Anthropic and OpenAI APIs, OpenAI-compatible services, and an HTTP webhook contract for any other hosted or local AI agent.

## Install

1. Copy [`examples/review.yml`](examples/review.yml) to `.github/workflows/review.yml` in the repository you want to review.
2. Add the selected provider key as a repository Actions secret named `REVIEW_API_KEY`.
3. Set `REVIEW_PROVIDER` and any adapter-specific values as repository Actions variables.
4. Open or update a pull request.

The reusable action can also be added directly:

```yaml
- name: Review pull request
  uses: wuisabel-gif/pr-reviewer@v0.2.0
  with:
    api-key: ${{ secrets.REVIEW_API_KEY }}
    provider: ${{ vars.REVIEW_PROVIDER || 'anthropic' }}
    model: ${{ vars.REVIEW_MODEL }}
```

Pinning a full commit SHA instead of a release tag provides the strongest supply-chain protection. The installation workflow uses `pull_request_target` but never checks out or executes the pull request head; it treats the fetched diff only as review input.

## How it works

1. The workflow triggers on `pull_request_target` events and runs the trusted, versioned reviewer action.
2. The binary fetches the unified diff, changed-file contents, and direct imports from the pull request's head revision. Context is byte-bounded and binary files are skipped.
3. It loads `REVIEW.md` from the trusted base revision, so a pull request cannot alter its own review rules.
4. It runs the configured number of independent review passes. When voting is enabled, only findings reported at the same file and line by the configured threshold survive.
5. It removes findings already posted by this action on earlier pushes, then validates remaining line anchors against the diff.
6. Valid findings become line comments. Findings outside the diff or comment limit are included in the summary, and a rejected batch falls back without losing findings.

## v0.2 features

- **Consensus reviews:** set `REVIEW_PASSES` from 1 to 7. `REVIEW_VOTE_THRESHOLD` defaults to a strict majority.
- **Cross-push deduplication:** inline comments contain stable, hidden location fingerprints that suppress repeat comments on later pushes.
- **Repository context:** changed text files and resolvable direct Rust, JavaScript/TypeScript, and Python imports are sent with the diff under a configurable byte budget.
- **Trusted rules:** a root-level `REVIEW.md` on the base branch is appended as review policy.
- **Benchmarks:** JSON suites replay known diffs and report location-level precision and recall.

Multiple passes increase model usage proportionally, so the default remains one pass. For majority voting, start with `REVIEW_PASSES=3`; the default threshold will be 2.

## Review rules

Add `REVIEW.md` to the repository's default branch to define project-specific review policy:

```markdown
# Review policy

- Treat authentication and authorization regressions as high severity.
- Ignore generated files under `src/generated/`.
- Database migrations must be backward compatible.
```

## Provider setup

Choose an adapter and add its API key as a repository secret named `REVIEW_API_KEY` under Settings, Secrets and variables, Actions. A local unauthenticated service or webhook does not need this secret. `REVIEW_PROVIDER` defaults to `anthropic`; other adapters require it explicitly. The `GITHUB_TOKEN` is supplied automatically and the example workflow grants only `contents: read` and `pull-requests: write`.

| Adapter | `REVIEW_PROVIDER` | Required configuration | Typical services |
|---|---|---|---|
| Anthropic Messages | `anthropic` | `REVIEW_API_KEY`; optional `REVIEW_MODEL` | Claude |
| OpenAI Responses | `openai` or `openai-responses` | `REVIEW_API_KEY`; optional `REVIEW_MODEL` | OpenAI GPT models |
| OpenAI-compatible Chat Completions | `openai-compatible` | `REVIEW_BASE_URL`, `REVIEW_MODEL`; API key when required | OpenRouter, Groq, Mistral, xAI, DeepSeek, Ollama, LM Studio, and compatible gateways |
| Generic webhook | `webhook` | `REVIEW_ENDPOINT`; optional `REVIEW_API_KEY` and `REVIEW_MODEL` | Any agent exposed through an HTTP adapter |

To run it by hand against any PR:

```bash
GITHUB_TOKEN=ghp_... ANTHROPIC_API_KEY=sk-ant-... \
GITHUB_REPOSITORY=owner/repo PR_NUMBER=42 \
cargo run --release
```

Or run with OpenAI:

```bash
GITHUB_TOKEN=ghp_... OPENAI_API_KEY=sk-... \
GITHUB_REPOSITORY=owner/repo PR_NUMBER=42 \
REVIEW_PROVIDER=openai cargo run --release
```

Run an OpenAI-compatible local or hosted model:

```bash
GITHUB_TOKEN=ghp_... REVIEW_API_KEY=provider-key \
GITHUB_REPOSITORY=owner/repo PR_NUMBER=42 \
REVIEW_PROVIDER=openai-compatible \
REVIEW_BASE_URL=https://provider.example/v1 REVIEW_MODEL=provider-model \
cargo run --release
```

## Universal webhook contract

Use `REVIEW_PROVIDER=webhook` for an agent that does not implement an OpenAI-compatible API. The reviewer sends a JSON object containing:

```json
{
  "task": "pull_request_review",
  "model": "optional-model-name",
  "system": "review instructions",
  "repository": "owner/repo",
  "diff": "unified diff",
  "context": "changed files and direct imports",
  "rules": "trusted REVIEW.md contents",
  "output_schema": { "type": "object" }
}
```

The webhook should return the normalized review directly:

```json
{
  "summary": "One-paragraph summary",
  "findings": [
    {
      "path": "src/example.rs",
      "line": 42,
      "severity": "high",
      "comment": "Problem and suggested fix"
    }
  ]
}
```

Responses wrapped in `review`, `output`, `result`, or `data` are also accepted, including JSON encoded as a string. For any other response shape, set `REVIEW_RESPONSE_JSON_POINTER` to the JSON Pointer locating the normalized review.

## Configuration

- `REVIEW_PROVIDER`: `anthropic`, `openai-responses`, `openai-compatible`, or `webhook`. Aliases include `claude`, `openai`, `openai-chat`, `chat-completions`, and `custom`.
- `REVIEW_MODEL`: provider-specific model. It defaults to `claude-sonnet-4-6` for Anthropic and `gpt-5.6-sol` for OpenAI Responses; it is required for OpenAI-compatible services and optional for webhooks.
- `REVIEW_API_KEY`: provider credential used by the supplied GitHub Actions workflow.
- `ANTHROPIC_API_KEY`: backward-compatible alternative to `REVIEW_API_KEY` for local Anthropic runs.
- `OPENAI_API_KEY`: backward-compatible alternative to `REVIEW_API_KEY` for local OpenAI and OpenAI-compatible runs.
- `REVIEW_BASE_URL`: base URL for OpenAI-compatible APIs. It falls back to `OPENAI_BASE_URL`, then `https://api.openai.com/v1`.
- `REVIEW_ENDPOINT`: exact URL for the generic webhook adapter.
- `REVIEW_AUTH_HEADER`: credential header for OpenAI-compatible and webhook requests; defaults to `Authorization`.
- `REVIEW_AUTH_SCHEME`: credential prefix; defaults to `Bearer`. Set it to `none` for raw-key headers such as `api-key`.
- `REVIEW_RESPONSE_JSON_POINTER`: optional RFC 6901 JSON Pointer for extracting a normalized review from a custom webhook response.
- `REVIEW_PASSES`: independent model calls per review, from 1 to 7; defaults to `1`.
- `REVIEW_VOTE_THRESHOLD`: passes that must report the same path and line; defaults to a strict majority.
- `REVIEW_CONTEXT_BYTES`: repository-context budget; defaults to `60000`, and `0` disables context fetching.
- `REVIEW_BOT_LOGIN`: only this author's hidden fingerprints are trusted for deduplication; defaults to `github-actions[bot]`.
- Diff and line-comment limits are bounded internally to control request and GitHub API sizes.

## Benchmarking

Create a suite using [`benchmarks/example.json`](benchmarks/example.json), then run it with any configured provider:

```bash
REVIEW_PROVIDER=openai OPENAI_API_KEY=sk-... \
cargo run --release -- --benchmark benchmarks/example.json
```

Each expected finding is matched by `path` and new-side `line`. The runner prints aggregate precision and recall as JSON, making it suitable for comparing providers, prompts, vote thresholds, and future releases.

## License

Licensed under the [MIT License](LICENSE).
