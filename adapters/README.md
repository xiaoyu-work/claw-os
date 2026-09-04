# Command-line tool adapters

Each directory wraps one external command-line tool as a Claw OS App. Adapters
use the same signed `app.json` contract, public `claw-os-sdk`, authenticated
MCP Gateway, capability derivation, sandbox, and audit path as every other App.
There is no separate adapter or generic MCP registration format.

## Layout

```text
adapters/
  <id>/
    app.json       # App identity, MCP tools, dependencies, and exact needs
    main.py        # name-only handler bindings through claw_os_sdk.mcp
    test_main.py   # command mapping and error tests
```

`app.json.mcp.tools[]` is the only source of tool names, descriptions,
arguments, defaults, caller access, and capability needs. Runtime code binds
only implementation functions:

```python
from claw_os_sdk.mcp import App

app = App.from_manifest()

@app.tool("archive.list")
def list_archive(path: str) -> list[str]:
    ...

app.serve()
```

The App Host points `COS_APP_MANIFEST` at the verified package snapshot and
injects authenticated call context outside business arguments. Direct
code-authored schemas and unauthenticated calls are rejected.

Adapters are not included in a Debian package yet. For development, install
one through the normal App installer:

```bash
cos app install adapters/qpdf --dev-trust
```

Production packages require publisher provenance; see
[`../docs/extension-provenance.md`](../docs/extension-provenance.md).

## Testing

From the repository root:

```bash
PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \
  python3 -m pytest -q adapters
```
