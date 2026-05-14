"""Skeleton adapter — copy this directory to ``adapters/<your-name>/``,
adjust ``manifest.json``, and fill in the tool implementations below.

The discovery layer keeps ``enabled: false`` here so the template
itself never registers in a running agent. Set it to ``true`` on the
copy.
"""

from __future__ import annotations

import os
import pathlib
import sys

# Bootstrap import path for ``_lib.serve``. Order: explicit env override
# (used by tests + packaging), then repo layout, then common install
# layouts. First hit wins.
_HERE = pathlib.Path(__file__).resolve().parent
_CANDIDATES = [
    pathlib.Path(os.environ["CLAW_PYTHON_LIB"]) if os.environ.get("CLAW_PYTHON_LIB") else None,
    _HERE.parent.parent / "apps",
    pathlib.Path("/opt/claw/python"),
    pathlib.Path("/usr/lib/claw/python"),
]
for cand in _CANDIDATES:
    if cand and (cand / "_lib").is_dir():
        sys.path.insert(0, str(cand))
        break

from _lib.serve import App  # noqa: E402  — sys.path bootstrap above.


app = App()


@app.tool(
    "example.echo",
    summary="Return the input text unchanged. Replace with real tools.",
    args={"text": {"type": "string", "description": "Text to echo."}},
    required=["text"],
)
def echo(text: str) -> str:
    return text


if __name__ == "__main__":
    app.serve()
