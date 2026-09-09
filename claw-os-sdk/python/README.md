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
| `claw_os_sdk.mcp`       | Manifest-bound App MCP server, call context, progress, and cancellation. |
| `claw_os_sdk.kernel`    | Explicit installed-CLI stdin transport with shared wire errors and cancellation. |
| `claw_os_sdk.claw_os_session` | Read / observe `COS_SESSION` from inside an app.                 |
| `claw_os_sdk.generated` | TypedDicts generated from `wire/v1/*.schema.json`.                     |

The package does not export `policy`. Capability gating
(`cos_runtime.policy.require`) and COW snapshots belong to the
OS-internal **`cos_runtime`** package, which is unavailable to
third-party SDK consumers. The `cos` kernel performs capability checks
when public SDK operations run.

## Expose App tools

Declare MCP tools once in `app.json`, then bind only their handlers:

```python
from claw_os_sdk.mcp import App, current_context

app = App.from_manifest()

@app.tool("mail.search")
def search(query: str) -> dict:
    context = current_context()
    context.raise_if_cancelled()
    context.report_progress(1, total=2, message="Searching")
    return {"matches": find_messages(query)}

app.serve()
```

`App.from_manifest()` reads the immutable manifest snapshot selected by the
App Host through `COS_APP_MANIFEST`. MCP-first calls require the Gateway's
versioned call context; caller identity never comes from tool arguments.
The runtime validates arguments and applies manifest defaults before calling
the handler. `report_progress()` is a no-op when the caller did not request
progress.

## Controlled primitive transport

`kernel.call_json_with_stdin_binary(binary, args, data, *,
deadline_unix_ms, check_cancelled=None)` sends bounded bytes to an explicit
absolute CLI path (installed Claw OS uses `/usr/local/bin/cos`).
`args` is the complete primitive argv; the SDK adds `--wire=1`. Pass the
authenticated MCP context's `raise_if_cancelled` callback and no later than
its deadline. The inherited broker session supplies authority, never stdin,
caller labels or environment changes.

It returns a decoded object or raises `KernelDenied` with the original
structured `.payload`, or `KernelUnavailable` for transport/decode failures.
Cancellation and deadlines kill/reap the CLI, but cannot undo an already
accepted OS mutation. Do not automatically retry an indeterminate mutation.
This helper is not an App dispatcher or a capability grant.

## AI support

`ai.chat` is the public model API. Passing `origin="external-content"`
automatically selects `ai.chat.untrusted`. Unsupported modalities are not
published as placeholder APIs.

## Configuration

| Env var          | Effect                                                                |
|------------------|-----------------------------------------------------------------------|
| `CLAW_COS_BIN`   | Path to the `cos` binary. Defaults to looking up `cos` in `$PATH`.    |
| `COS_APP_ID`     | App id, used by `ai.chat` / `tools.call`.                             |
| `COS_APP_MANIFEST` | Verified manifest snapshot used by `mcp.App.from_manifest()`.       |

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
