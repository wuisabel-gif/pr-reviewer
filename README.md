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
  uses: wuisabel-gif/pr-reviewer@v0.1.1
  with:
    api-key: ${{ secrets.REVIEW_API_KEY }}
    provider: ${{ vars.REVIEW_PROVIDER || 'anthropic' }}
    model: ${{ vars.REVIEW_MODEL }}
```

Pinning a full commit SHA instead of a release tag provides the strongest supply-chain protection. The installation workflow uses `pull_request_target` but never checks out or executes the pull request head; it treats the fetched diff only as review input.

## How it works

1. The workflow triggers on `pull_request_target` events and runs the trusted, versioned reviewer action.
2. The binary reads the PR number from the Actions event payload, then fetches the unified diff from the GitHub API.
3. It parses the diff to build a map of which lines GitHub will actually accept comments on (added and context lines inside hunks). This prevents 422 errors from the Reviews API when the model hallucinates a line number.
4. It sends the diff through the configured provider adapter and requests a summary plus findings with `path`, `line`, `severity`, and `comment`. OpenAI Responses uses a strict JSON schema. Other adapters use the same normalized output contract. The shared prompt tells the model to skip style nitpicks and only flag real bugs.
5. Valid findings become line comments on the PR review. Findings whose line numbers fall outside the diff get folded into the summary body instead of being dropped. If GitHub still rejects the review, it falls back to posting the summary alone.

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
- `MAX_DIFF_BYTES` and `MAX_COMMENTS` are constants in `src/main.rs`. Large diffs get truncated with a marker so the model knows it saw a partial view.

## Ideas for v2

- Majority voting: run the review N times and keep only findings that appear in most passes to suppress false positives.
- Dedup across pushes: fetch existing review comments and skip findings the bot already made, so force-pushes don't spam.
- Repo context: for each changed file, also send the full file content and its direct imports instead of just the diff.
- A `REVIEW.md` rules file in the repo root that gets appended to the system prompt.
- Benchmarking: replay merged PRs with known bugs and score recall/precision.

## License

Licensed under the [MIT License](LICENSE).
