# Agent Media Module

## Purpose

`media/` routes vision, speech, audio, and image-related agent tasks through
configured local or cloud capabilities.

## Responsibilities

- Normalize media requests and provider outputs.
- Select configured media implementations.
- Preserve capability, credential, and size/timeout boundaries.
- Keep media-specific payloads out of the conversational core.

## Key Files

| Path | Role |
| --- | --- |
| `vision/` | Image analysis and routing |
| `voice/` | Voice input/output and playback |
| `imagegen.rs` | Image generation |
| `factory.rs` | Media provider construction |

## Dependencies

Media callers use provider interfaces/factories. External media and generated
content are untrusted; credentials remain in the credential/config layers and
are never embedded in model-visible errors.

## Tests

```bash
cargo test -p cos agent::media:: -- --test-threads=1
```
