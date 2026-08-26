# SDK Module

## Purpose

`claw-os-sdk/` defines the public, language-neutral app/agent contract and its
Rust, Python, Node, and Go bindings.

## Responsibilities

- Maintain versioned wire types and operation/capability schemas.
- Provide public SDK calls without exposing internal broker details.
- Keep language bindings behaviorally compatible.
- Release every language binding at the same SDK SemVer through GitHub.
- Generate, rather than hand-edit, generated bindings.

## Key Files

| Path | Role |
| --- | --- |
| `wire/` | Versioned contract and code generation |
| `rust/` | Rust public SDK |
| `python/` | Python public SDK |
| `node/` | Node public SDK |
| `go/` | Go public SDK |
| `python/src/claw_os_sdk/generated.py` | Generated Python wire bindings |
| `../.github/workflows/publish-sdk-release.yml` | GitHub-only multi-language SDK release |

`cos-runtime/` is a separate internal package for bundled apps; public apps
must not depend on its policy/runtime internals.

## Dependencies

Wire schema is the source of truth. Core and every language SDK consume it.
Serialization changes stay backwards compatible unless introduced under a new
wire version.

## Tests

Rust SDK unit tests mirror `rust/src/` under `rust/test/unit/`; production files
only contain cfg(test) include declarations.

Regenerate from this directory after wire changes:

```bash
cd claw-os-sdk
python3 wire/codegen.py
```

Then run the affected language tests plus the repository Python suite:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q claw-os-sdk/python/src
cargo test -p claw-os-sdk
```
