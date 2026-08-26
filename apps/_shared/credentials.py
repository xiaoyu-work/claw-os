"""Capability-gated credential loading for bundled Apps."""

import json
import os
import shutil
import subprocess

from .env_scrub import scrub_env


def load_credential(name, *, namespace="default", timeout=10):
    """Return ``(value, error)`` without exposing secret material in errors."""
    if not isinstance(name, str) or not name.strip():
        return None, "credential name required"
    if not isinstance(namespace, str) or not namespace.strip():
        return None, "credential namespace required"

    cos_bin = os.environ.get("COS_BIN") or shutil.which("cos")
    if not cos_bin:
        return None, "cos binary not found"
    try:
        result = subprocess.run(
            [
                cos_bin,
                "credential",
                "load",
                name,
                "--namespace",
                namespace,
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=scrub_env(),
        )
    except FileNotFoundError:
        return None, "cos binary not found"
    except subprocess.TimeoutExpired:
        return None, f"credential load timed out after {timeout}s"
    except OSError as exc:
        return None, f"credential store unavailable: {exc}"

    if result.returncode != 0:
        return None, (
            f"credential load returned {result.returncode} "
            "(stderr suppressed to protect secret material)"
        )
    try:
        payload = json.loads(result.stdout or "")
    except json.JSONDecodeError:
        return None, "credential response was not valid JSON"
    if not isinstance(payload, dict):
        return None, "credential response was not an object"
    value = payload.get("value")
    if not isinstance(value, str) or not value.strip():
        return None, f"credential {namespace}/{name} has no value"
    return value.strip(), None


__all__ = ["load_credential"]
