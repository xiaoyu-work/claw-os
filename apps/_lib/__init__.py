"""Shared helpers for Claw OS Python apps.

Modules in this package are intentionally tiny: an app is supposed to
`from _lib import policy` (and similar) at the top of its `main.py`
and otherwise depend only on the Python standard library and its own
declared `python` dependencies in `app.json`.
"""
