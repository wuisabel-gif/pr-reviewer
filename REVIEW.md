# Review policy

- Report functional, security, reliability, or compatibility defects; skip style-only feedback.
- Provider adapters must preserve the normalized `summary` and `findings` response contract.
- Privileged workflows must never execute code from a pull request head.
- New configuration must be reflected in `action.yml`, the example workflow, and the README.
- Changes to diff parsing, consensus, deduplication, or response parsing require focused tests.
