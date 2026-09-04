"""Skeleton adapter — copy this directory to ``adapters/<your-name>/``,
adjust ``app.json``, and fill in the tool implementations below."""

from __future__ import annotations

import pathlib

from claw_os_sdk.mcp import App


_HERE = pathlib.Path(__file__).resolve().parent
app = App.from_manifest(_HERE / "app.json")


@app.tool("example.echo")
def echo(text: str) -> str:
    return text


if __name__ == "__main__":
    app.serve()
