# Engine Package Module

## Purpose

`engine_pkg/` describes downloadable local model engines, their sources,
versions, platform compatibility, installation, and verification.

## Responsibilities

- Maintain known engine metadata and source resolvers.
- Download/install engines into managed locations.
- Verify checksums/layout and report compatibility explicitly.
- Expose engine package lifecycle to model/runtime callers.

## Key Files

| Path | Role |
| --- | --- |
| `mod.rs` | Registry and package lifecycle |
| `manifest.rs` | Engine package manifest types |
| `sources/` | Source-specific resolution/download logic |

## Dependencies

Model engine loaders consume verified package outputs; they do not duplicate
download/version logic. Unknown platform or validation failure is an error, not
a success-shaped fallback.

The Qwen3 image feature installs the validated ONNX Runtime and ORT GenAI
versions declared in `mod.rs` into the same system registry. GenAI's upstream
archive does not contain ONNX Runtime. Model loaders retain the registered
dependency instead of relying on ambient library search paths.

## Tests

```bash
cargo test -p cos engine_pkg:: -- --test-threads=1
```
