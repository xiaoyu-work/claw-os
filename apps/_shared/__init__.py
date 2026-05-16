"""Shared helpers for the Python apps under ``apps/``.

Each app runs as a standalone Python process, so this package is
imported via a tiny ``sys.path`` shim at the top of each ``main.py``
that needs it::

    import os, sys
    sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
    from _shared.paths import safe_realpath
    from _shared.atomic import atomic_write_bytes, atomic_write_json
    from _shared.env_scrub import scrub_env

The helpers here are intentionally tiny — anything bigger should live
in ``cos-runtime`` or the SDK proper.
"""
