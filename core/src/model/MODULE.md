# Model Module

## Purpose

`model/` owns local model package/runtime selection and task-specific model
implementations outside the conversational provider layer.

## Responsibilities

- Resolve and validate installed model engine packages.
- Build embedding and other task runtimes.
- Keep architecture/runtime compatibility explicit.
- Expose model tasks to core and CLI callers.

## Key Files

| Path | Role |
| --- | --- |
| `tasks/` | Embedding and task-specific model interfaces/implementations |
| `engines/` | llama.cpp, ONNX Runtime, ORT GenAI backends |
| `compat.rs` | Host/model compatibility |
| `import.rs` | Model import lifecycle |
| `mod.rs` | CLI/task dispatch and exports |

## Dependencies

Task callers depend on model-task interfaces, not engine implementation
details. Engine packages are validated before loading; local failure must not
silently fall back to fake embeddings or success-shaped output.

## Tests

```bash
cargo test -p cos model:: -- --test-threads=1
```
