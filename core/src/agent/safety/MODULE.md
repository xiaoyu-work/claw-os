# Agent Safety Module

## Purpose

`safety/` classifies risky content/paths, redacts secrets, checks external
dependencies, and supplies reusable safety decisions to agent workflows.

## Responsibilities

- Detect credentials and sensitive text without logging secrets.
- Classify unsafe file paths/extensions and external content.
- Integrate security/advisory checks.
- Return explicit allow/caution/deny decisions with actionable categories.

## Key Files

| Path | Role |
| --- | --- |
| `redact.rs` | Secret/PII detection and redaction |
| `file_safety.rs` | File/path classification |
| `osv.rs` | Vulnerability/advisory integration |

## Dependencies

Safety helpers are pure where possible and sit before persistence, prompts, or
side effects. Do not convert uncertainty or provider failure into an implicit
allow decision.

## Tests

```bash
cargo test -p cos agent::safety:: -- --test-threads=1
```
