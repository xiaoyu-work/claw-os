# claw-os-sdk (Python)

The official Python SDK for Claw OS. Every Python app under
`apps/<name>/` imports this package as `claw_os_sdk`.

## Install

```sh
pip install \
  https://github.com/xiaoyu-work/claw-os/releases/download/sdk-v0.1.0/claw_os_sdk-0.1.0-py3-none-any.whl
```

On a Claw OS system this is pre-installed (sources are baked into
the rootfs at `/usr/lib/cos/python/claw_os_sdk/`).

## Use

```python
from claw_os_sdk import ai

def handle_summarize(args):
    with open(args["path"]) as fh:
        body = fh.read()
    result = ai.chat(prompt=body, origin="external-content", max_units=2000)
    return {"summary": result.text, "usage": result.usage}
```

## What's in it

| Module                  | Purpose                                                                |
|-------------------------|------------------------------------------------------------------------|
| `claw_os_sdk.ai`        | Stable `chat` / `chat-untrusted` access through `cos ai chat`.          |
| `claw_os_sdk.tools`     | `cos ai tool <name>` — fulfil catalog tools the model proposed.        |
| `claw_os_sdk.gui`       | Desktop GUI bootstrap and kernel-provided launch context.              |
| `claw_os_sdk.serve`     | `App.run(...)` — boilerplate for an app's main loop.                   |
| `claw_os_sdk.claw_os_session` | Read / observe `COS_SESSION` from inside an app.                 |
| `claw_os_sdk.generated` | TypedDicts generated from `wire/v1/*.schema.json`.                     |

The package does not export `policy`. Capability gating
(`cos_runtime.policy.require`) and COW snapshots belong to the
OS-internal **`cos_runtime`** package, which is unavailable to
third-party SDK consumers. The `cos` kernel performs capability checks
when public SDK operations run.

## AI support

- **Stable:** `ai.chat`. Passing `origin="external-content"`
  automatically selects `ai.chat.untrusted`.
- **Compatibility only:** embed, image, vision, audio, and video helpers
  retain their signatures but are deprecated, experimental, and currently
  unsupported. They raise `ai.AiUnsupported` before invoking `cos`.

## Configuration

| Env var          | Effect                                                                |
|------------------|-----------------------------------------------------------------------|
| `COS_BIN`        | Path to the `cos` binary. Defaults to looking up `cos` in `$PATH`.    |
| `COS_APP_ID`     | App id, used by `ai.chat` / `tools.call`.                             |

## Wire protocol

This package implements wire protocol v1. See
[`../wire/v1/README.md`](../wire/v1/README.md) for the full spec.
Regenerate typed structs with:

```sh
python3 ../wire/codegen.py
```

## History

This package was previously known as `_lib` and lived under
`apps/_lib`. It has been promoted to the public, multi-language SDK
at `claw-os-sdk/python` and renamed. There is no compatibility shim
— claw-os is pre-1.0 and breaking changes are allowed.
