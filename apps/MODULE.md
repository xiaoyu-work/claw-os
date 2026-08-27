# Apps Module

## Purpose

`apps/` contains bundled Python operations exposed through `cos app`. Each app
is declarative at discovery time and executable only when an operation is
invoked.

## Responsibilities

- Own every operation, argument, dependency, AI use, and capability need in
  `app.json`; entrypoints must not implement `_schema()` or `__schema__`.
- Declare each argument's positional/flag binding, exact numeric kind, and
  runtime default so validated argv and capability derivation cannot diverge.
- Keep optional gateway destinations as flags after required message text;
  fixed path scopes use absolute or `~/` forms, never environment placeholders.
- Validate untrusted input before requesting policy/capability authority.
- Derive HTTP `net.dial` scopes with `_shared.safe_http.host_scope` so App
  checks include the same effective port as manifest `url-host` authority,
  including redirects, IDNA domains, legacy IPv4 forms, and bracketed IPv6.
  Rust and Python parity is defined by `_shared/url_host_scope_vectors.json`.
- Declare destructive confirmation booleans as required with `choices: [true]`;
  omission and explicit false must fail before authority is resolved.
- Use `required_when` for conditional confirmation requirements; it references
  an earlier argument through the same closed condition model as capability
  needs.
- Return structured JSON-compatible results.
- Return constrained `setup.agent_action` metadata for Agent-resumable
  authorization failures; never place credentials or tokens in that metadata.
- Use the public SDK for OS/agent access and `cos_runtime` only for bundled-app
  policy/runtime helpers.

## Key Files

| Path | Role |
| --- | --- |
| `<id>/app.json` | App identity and operation/capability contract |
| `<id>/main.py` | Behavior-only `run(command, args)` implementation |
| `<id>/test_main.py` | App behavior, validation, and scope tests |
| `_shared/` | Shared safe filesystem/HTTP/process helpers |
| `gateway/` | External messaging gateways and shared gateway safety helpers |
| [`../docs/app-development.md`](../docs/app-development.md) | Normative app/manifest development contract |

## Dependencies

Apps do not import model-provider SDKs or own provider credentials. AI calls go
through the Claw OS SDK/agent gate. Bundled capability checks use
`cos_runtime.policy`; operation `needs` in `app.json` must match runtime checks.
Schema/listing paths are generated from `app.json` and must not execute
`main.py`. Unknown operations are rejected by the kernel before dispatch;
unknown flags are rejected during manifest binding; entrypoints retain an
unknown-operation error for direct unit invocation.

## Tests

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps

# One app
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q apps/<id>/test_main.py
```

When available, also run `cos app lint <id>`. Tests should verify that invalid
arguments are rejected before policy checks and that requested scopes are
exact.

List-based Python handlers consume bridge-canonical argv through
`apps/canonical_argv.py`. Do not add another local flag grammar: declare closed
choices, repeatability, defaults, stdin forwarding, and capability conditions
in `app.json`, then reuse the shared parser compatibility helpers. Historical
short/long flags and destination positionals must use manifest `aliases` or
`positional_alias`, never parser-only exceptions.
The parser preserves post-`--` positional classification; never strip the
delimiter and re-run local flag detection. Stdin is closed unless the
top-level CLI explicitly supplies `--stdin` and the operation opts in.
