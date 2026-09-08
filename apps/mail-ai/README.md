# Mail AI — shared business foundation

This is **M1**, the first foundation of the
[Mail product redesign](../../docs/app-product-redesign.md). The App identity
remains `mail-ai`. It is not a unified mailbox, a completed Mail product, or a
completed upstream fork. M2 defines the mailbox contract, M3 integrates the
pinned upstream source, and M4 merges `email`, `mail-ai`, and `gateway-email`
with an atomic identity and data/consent cutover.

The Thunderbird [MailExtension](../../extensions/claw-mail-ai/) and the
system Agent's authenticated MCP tools call the same typed Python functions in
[`main.py`](main.py). There are no App-to-App calls, subprocess dispatchers,
argv parsers, legacy aliases, or duplicated operation schemas. Thunderbird is
a UI dependency, not a dependency of the headless business implementation.

## Business contract

[`app.json`](app.json) is the sole MCP schema. Optional CLI `binding` metadata
stays on its MCP arguments; there is no separate `operations` table.

| Function / MCP suffix | Required arguments | Optional arguments |
| --- | --- | --- |
| `summarize` | `body` | `subject`, `sender`, `lang` |
| `smart_reply` | `thread` | `subject`, `sender`, `intent`, `lang` |
| `smart_compose` | `intent` | `subject`, `recipient`, `draft`, `style`, `lang` |
| `translate` | `text`, `target` | — |
| `triage` | At least one nonblank `subject`, `sender`, `snippet` | `has_attachments` |
| `chat` | `question` | `context_json`, `lang` |

Every argument is a string except `has_attachments`, which is a boolean.
`style` is `formal`, `casual`, or `short`; language defaults to `en`.
`context_json` is a JSON-encoded array of objects with optional string fields
`sender`, `subject`, `date`, `snippet`. No other context fields are accepted.
The complete array is validated before the first 20 messages are selected.
Unknown fields, wrong types, missing required inputs, blank content, and
malformed context fail before capability checks, model calls, or memory writes.

The existing body/thread/draft limits remain 12,000/24,000/4,000 characters.
Triage uses at most 1,000 snippet characters; chat uses at most 400 per message.
Summary quote stripping, response repair, output shaping, and per-operation AI
unit caps are retained. Chat answers only from the supplied context; it does not
query a mailbox itself.

## Two transports, one implementation

**MCP:** [`server.py`](server.py) uses public `App.from_manifest`, `App.tool`,
and `App.serve`. The SDK validates manifest arguments and authenticated call
metadata. A transport-only result encoder preserves MCP `isError` and structured
error details; it does not translate argument names or construct argv.

**Native Messaging:** [`native_host.py`](native_host.py) accepts a four-byte
little-endian length prefix followed by UTF-8 JSON:

```json
{"id":"request-1","verb":"summarize","args":{"body":"Please review.","sender":"alex"}}
```

Success is `{"id":"request-1","ok":true,"result":{...}}`; business failures
are `{"id":"request-1","ok":false,"error":"...","detail":{...}}`.
The envelope must contain exactly `id`, `verb`, and `args`; the first two
are nonempty strings and `args` is an object. Argument spellings are identical
to the table above; underscore/hyphen/`from` aliases are not supported.

Requests call the business functions in process. This transport does not
invoke `cos app`, manufacture an MCP identity, or access SDK authentication
internals. Bad requests return an error and leave the host healthy. Truncated,
zero/oversized, non-UTF-8, or invalid-JSON frames produce a diagnostic on stderr
and a nonzero exit. Normal EOF exits successfully. Unexpected request errors
do not expose tracebacks or message bodies.

## Authority and deployment

The root-owned `claw-mail-ai-host` launcher is responsible for verifying the
Thunderbird parent and registering the provenance-checked `mail-ai` App session.
The canonical installed App directory is `/usr/lib/cos/apps/mail-ai`; there is
no separate executable App copy at `/usr/lib/cos/mail-ai`.

The launcher starts `/usr/lib/cos/apps/mail-ai/native_host.py` with isolated
Python. The host explicitly adds its own canonical directory and
`/usr/lib/cos/python` for the packaged SDK and runtime; it does not depend on
`PYTHONPATH` or search legacy install roots. Source-checkout execution uses only
the matching repository SDK/runtime source directories.

All model calls require exact `ai.chat.untrusted` authority and use
`claw_os_sdk.ai.chat(origin="external-content")`. Core retains provider
credentials, owner identity, consent, budgets, safety, and audit. The manifest
retains strict safety and a 500,000-unit monthly budget. Only summaries and the
existing notable-triage cases write memory under the `mail-ai` self scope;
only `memory.MemoryError` is ignored for that optional write.

The installed `claw-os-agent` package supplies the canonical App, shared Python
SDK/runtime, root-owned launcher, and protocol-matched Thunderbird XPI at
`/usr/lib/thunderbird/distribution/extensions/claw-mail-ai@claw.os.xpi`.
Packaging both protocol peers together keeps their argument contracts aligned
on APT upgrades without making Thunderbird a headless App dependency.
The canonical-argument transition advances both manifests to `0.2.0` while
retaining their identities, so Thunderbird recognizes the packaged XPI update.
Build and install the matching package
through the normal [packaging workflow](../../packaging/README.md) first.
Neither the [`claw-mail-ai` rootfs feature](../../rootfs/features/claw-mail-ai/)
nor [`tools/install-mail-ai.sh`](../../tools/install-mail-ai.sh) copies or
overwrites those package-owned files; preserving their package provenance is
required.

The rootfs feature supplies the Thunderbird UI integration and reuses that
installed package, including its XPI. The development installer only registers
the native host and Thunderbird policies after requiring the matching
package-owned App, SDK/runtime, launcher, and XPI. It does not regenerate or
shell-copy an extension from source, accept a custom host binary, or silently
fall back to a source App copy. Rebuild/reinstall the package when either the
business implementation or extension UI changes.

`python3 -I apps/mail-ai/native_host.py --probe` is a side-effect-free
source-checkout import/operation-list diagnostic, not an installed integration,
model, or mailbox test.

## Validation

From the repository root on Linux/WSL:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/mail-ai/test_main.py extensions/claw-mail-ai/test_contract.py
```

Tests compare real framed Native Messaging and SDK MCP requests for all six
operations using deterministic injected AI/policy/memory effects, validate
failure shapes and malformed input, and execute the JavaScript UI request
builders with Node and Thunderbird stubs. They do not require Thunderbird or
an external model, and do not replace core launcher/session/provenance tests.
