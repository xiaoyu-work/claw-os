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
| `local_embedding.rs` | Versioned fixed-Qwen3 CPU pipe for installed file indexers and Root-produced non-root owner helpers |

## Dependencies

Task callers depend on model-task interfaces, not engine implementation
details. Engine packages are validated before loading; local failure must not
silently fall back to fake embeddings or success-shaped output.

The file-index pipe uses only the installed Qwen3 model slot and managed local
engines, independent of conversational/cloud embed configuration. It carries
no file path, capability, provider credential or executable selector. Root
parentage permits only fixed non-root CPU inference; Workspace file authority
remains at the caller's broker boundary.

ORT GenAI preloads and retains the registered ONNX Runtime dependency before
loading its native initializer. Both packages are required; installing GenAI
alone must return an actionable missing-dependency error, not let a foreign
exception abort the process. The image's validated pair is declared in
`engine_pkg/mod.rs`. File inference uses a 2048-token ceiling; the general
Qwen3 embedding task retains its existing separate token limit.

## Tests

```bash
cargo test -p cos model:: -- --test-threads=1
```
