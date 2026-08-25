# Internationalization Module

## Purpose

`i18n/` centralizes locale detection, localized strings, fallback behavior, and
translation catalog access for core user-facing output.

## Responsibilities

- Parse and normalize locale identifiers.
- Select deterministic fallback locales.
- Resolve localized messages without changing structured result fields.
- Keep CLI/app-facing localization behavior consistent.

## Key Files

| Path | Role |
| --- | --- |
| `locale.rs` | Locale parsing and fallback |
| `mod.rs` | Translation lookup and exports |

## Dependencies

Structured protocol keys and machine-readable values are not localized. Only
user-facing labels/messages pass through this module.

## Tests

```bash
cargo test -p cos i18n:: -- --test-threads=1
```
