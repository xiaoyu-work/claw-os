# App AI Tool Catalog

The Tool catalog is the **single source of truth** for what computer
operations a Claw OS App may ask an LLM to invoke through the kernel
AI gate. Every entry below is:

* **Verb-bound** — each Tool maps to exactly one capability verb
  (e.g. `fs.read`, `data.kv.read`). The kernel runs `caps::require`
  against the App's grant before any side-effect, so listing a Tool
  in `ai.tools[]` does *not* widen the App's permissions.

* **Identity-pinned** — every call goes through the same
  `enforce_identity` path as `cos ai chat`. The `--app <id>` flag
  must match `COS_APP_ID` injected by the kernel. Cross-App
  impersonation is impossible without kernel complicity.

* **Audited** — `cos ai tool` writes one `LlmRunRecord` per
  invocation to the same `<log_dir>/ai.jsonl` stream as `cos ai
  chat`, with `provider="kernel"`, `model="tool:<name>"`, and the
  derived caps verb. Operators can grep one file for everything an
  App did under the AI surface.

> **The gate never executes a tool call the model proposed.**
> `cos ai chat --tools <list>` only *exposes* tools to the model.
> Proposed calls come back in `ChatResult.tool_calls[]`; the App
> decides whether to fulfil any of them by shelling to
> `cos ai tool <name>` (or `tools.call(...)` from the Python SDK).
> Each fulfilment is its own kernel-mediated, audited call.

For the higher-level architecture and the line between `cos ai`
(App-facing primitive) and `cos agent` (kernel's own Agent product),
see [`docs/app-ai-integration.md`](./app-ai-integration.md).

> **App-defined tools live in a separate namespace.** The catalog
> below is the *kernel-provided* Tool set Apps consume. Apps can also
> *expose* their own Tools so the kernel agent can hold a stateful
> Session across multiple Apps — see
> [§12 in app-ai-integration.md](./app-ai-integration.md#12-app-session-tools-apps-as-mcp-servers).
> The two surfaces don't overlap: catalog tools are kernel-owned and
> shared; App-session tools are App-owned, declared in the manifest's
> `session.tools[]`, and reach the model under registry names like
> `app_kv__kv_get`.

---

## How an App uses the catalog

```python
from _lib import ai, tools

proposal = ai.chat(
    prompt="Summarise the file at /etc/hostname.",
    tools=tools.for_chat("fs.read_text"),
)

for call in proposal.tool_calls:
    try:
        result = tools.call(call.name, call.input)
    except tools.ToolDenied as e:
        # caps refused / unknown tool / bad args — already audited
        print("denied:", e.payload)
        continue
    # feed result.value into the next ai.chat turn however the App prefers
```

The App's `app.json` must declare every tool name in `ai.tools[]`:

```jsonc
{
  "id": "summarize",
  "ai": {
    "budget":  { "monthly_units": 5000 },
    "safety":  "strict",
    "origins": ["trusted"],
    "tools":   ["fs.read_text"]
  },
  "operations": {
    "summarize": {
      "needs": [
        { "verb": "ai.chat",
          "scope": { "kind": "fixed", "scope": { "kind": "name", "value": "*" } },
          "why":  "Summarise text the user pastes in." },
        { "verb": "fs.read",
          "scope": { "kind": "from-arg", "arg": "path" },
          "why":  "Read the file the model decides to summarise." }
      ]
    }
  }
}
```

Unknown names in `ai.tools[]` are rejected at install/launch time
(see `Manifest::validate_tools_against_catalog`); duplicates are
rejected by `Manifest::validate()`.

---

## Stability tiers

| Tier             | Meaning                                                                                                                                                |
|------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| **stable**       | Args/return shape is frozen. Will not be removed without a deprecation cycle.                                                                          |
| **experimental** | Shape may change between releases. Apps that depend on this tool MUST pin a kernel version or be prepared to handle `unknown tool` and shape changes. |

---

## Catalog

### `fs.read_text`  · stable

Read a UTF-8 text file. Returns the file body.

* **Verb** `fs.read`
* **Scope binding** `from-arg path` — the `path` argument is the
  scope the gate enforces. Listing this tool does not let the model
  read files outside the App's granted `fs.read` scope.
* **Args**

  | Field        | Type    | Required | Default     | Notes                                                               |
  |--------------|---------|----------|-------------|---------------------------------------------------------------------|
  | `path`       | string  | yes      | —           | Absolute path or `~`-relative.                                      |
  | `max_bytes`  | integer | no       | `1_048_576` | Hard cap on how much of the file to return. The result is truncated rather than rejected when the file is larger. |

* **Returns**

  | Field        | Type    | Notes                                          |
  |--------------|---------|------------------------------------------------|
  | `path`       | string  | Echo of the requested path.                    |
  | `bytes_read` | integer | Bytes actually returned in `content`.          |
  | `content`    | string  | UTF-8 body. Lossless if `truncated` is false.  |
  | `truncated`  | boolean | True iff the file was longer than `max_bytes`. |

* **Failure modes**
  * Path falls outside granted `fs.read` scope → caps denial
    (`denial_reason="caps_denied"`).
  * Path doesn't exist or isn't a regular file → tool-impl error.
  * File isn't valid UTF-8 → tool-impl error.

---

### `fs.list`  · stable

List entries in one directory level. Returns name + kind + size.

* **Verb** `fs.meta`
* **Scope binding** `from-arg path` — the directory must lie within
  the App's granted `fs.meta` scope.
* **Args**

  | Field         | Type    | Required | Default | Notes                                  |
  |---------------|---------|----------|---------|----------------------------------------|
  | `path`        | string  | yes      | —       | Directory to list.                     |
  | `max_entries` | integer | no       | `256`   | Maximum entries to include.            |

* **Returns**

  | Field       | Type    | Notes                                                |
  |-------------|---------|------------------------------------------------------|
  | `path`      | string  | Echo of the requested path.                          |
  | `entries`   | array   | One element per entry; see below.                    |
  | `truncated` | boolean | True iff more entries existed than `max_entries`.    |

  Each `entries[]` element:

  | Field  | Type    | Notes                                              |
  |--------|---------|----------------------------------------------------|
  | `name` | string  | File name (no directory component).                |
  | `kind` | string  | One of `"file"`, `"dir"`, `"symlink"`, `"other"`.  |
  | `size` | integer | Bytes (regular files only); omitted otherwise.     |

* **Failure modes**
  * Path falls outside granted `fs.meta` scope → caps denial.
  * Path doesn't exist or isn't a directory → tool-impl error.

---

### `kv.get`  · stable

Read a value from the App's per-App key-value store. Returns
`null` for missing keys.

* **Verb** `data.kv.read`
* **Scope binding** `name` — the gate matches the key against the
  App's granted `data.kv.read` scope. The store itself is per-App —
  one App cannot read another's keys regardless of grant — so the
  scope only controls *which* of the App's own keys the model is
  allowed to ask about.
* **Args**

  | Field | Type   | Required | Notes                                       |
  |-------|--------|----------|---------------------------------------------|
  | `key` | string | yes      | Non-empty. Sanitised on the kernel side.    |

* **Returns**

  | Field   | Type            | Notes                                  |
  |---------|-----------------|----------------------------------------|
  | `key`   | string          | Echo of the requested key.             |
  | `value` | string \| null  | `null` if the key has never been set.  |

* **Failure modes**
  * Key falls outside granted `data.kv.read` scope → caps denial.
  * Backing JSON is corrupt → tool-impl error.

---

## How to add a new Tool

Adding a Tool is intentionally five small edits in lockstep so the
catalog can never drift from the implementation:

1. **`core/src/ai/tools.rs`**
   * Add a `ToolDef { ... }` entry to `CATALOG`. Pick a stable name
     (`namespace.verb_phrase`) and the verb the gate should require.
   * Write the args / returns schemas as inline JSON Schema strings.
     Keep them strict — set `additionalProperties: false` on args.

2. **`derive_scope`** in the same file: add a match arm that
   extracts the scope from the tool's args. If the binding is a
   path, return `Scope::path(...)`; if it's a name, return
   `Scope::name(...)`.

3. **`execute_inner`** (also in `tools.rs`): add a match arm that
   calls your implementation function. Implementation should return
   `Result<serde_json::Value, String>`; the surrounding `execute`
   wraps it in a `ToolResult` and writes the audit row.

4. **Tests.** Add at least:
   * A `derive_scope_*` test confirming the arg-to-scope rule.
   * An `execute_*` test confirming malformed args are rejected with
     a clear error.
   * A schema-parse test (the existing `schemas_parse_as_json`
     covers the whole catalog automatically — it will fail loudly
     if your schema is malformed).

5. **This document.** Add a new section under "Catalog" with the
   verb, scope binding, args, returns, and failure modes. The Python
   SDK does not need to change — `tools.call(name, args)` is generic.

After landing the change:

* `cos ai tools` will start reporting the new entry.
* Apps must add the tool name to their `app.json`'s `ai.tools[]`
  before they can pass it to `cos ai chat --tools` — the kernel
  catalog check in `bridge.rs` will otherwise reject the App at
  launch.
* The audit stream will record `model="tool:<your_name>"` rows; no
  dashboard changes are needed.
