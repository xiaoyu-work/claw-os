"""Environment scrubbing for ``subprocess`` spawns.

Any child process we spawn out of the agent inherits the parent
environment by default. That environment routinely contains
provider API keys (``OPENAI_API_KEY``, ``ANTHROPIC_API_KEY``,
``GITHUB_TOKEN``…), OAuth tokens, AWS creds, GitHub installation
tokens, and so on. The agent must not hand those credentials to an
arbitrary command the user (or a model) asked it to run.

Use :func:`scrub_env` to derive a sanitised env dict to pass as
``env=`` on every ``subprocess.run`` / ``Popen`` call that spawns a
process without a specific need for those keys.

Policy
======

We use a **denylist** rather than a strict allowlist because:

* The kernel deliberately passes through several ``COS_*`` variables
  the child needs (``COS_SESSION``, ``COS_DATA_DIR``, ``COS_BIN``).
* Locale (``LANG``, ``LC_*``), terminal (``TERM``), time zone
  (``TZ``), display (``DISPLAY``, ``WAYLAND_DISPLAY``) and similar
  benign variables are required for usable child programs, and
  enumerating them as an allowlist is brittle.

The denylist below is broad: any variable name matching one of the
suffix / prefix patterns is dropped. Add to it if a new secret is
introduced — *never* add new patterns to the allowlist side.
"""

from __future__ import annotations

import os
import re
from typing import Mapping, MutableMapping, Optional


_SUFFIX_BLOCKED = (
    "_API_KEY",
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_PASSWD",
    "_PRIVATE_KEY",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_ACCESS_KEY",
    "_SECRET_KEY",
)

_PREFIX_BLOCKED = (
    "OPENAI_",
    "ANTHROPIC_",
    "GITHUB_",
    "GH_",
    "AWS_",
    "GOOGLE_",
    "GCP_",
    "GCLOUD_",
    "MICROSOFT_",
    "AZURE_",
    "CLAUDE_",
    "COPILOT_",
    "HF_",
    "HUGGINGFACE_",
    "MISTRAL_",
    "REPLICATE_",
    "GROQ_",
    "COHERE_",
    "TOGETHER_",
    "PERPLEXITY_",
    "DEEPSEEK_",
    "XAI_",
    "OPENROUTER_",
)

_EXACT_BLOCKED = frozenset(
    {
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CLAUDE_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "MISTRAL_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACE_TOKEN",
        "AZURE_OPENAI_KEY",
        "AZURE_OPENAI_API_KEY",
        # Container runtime / OAuth artefacts that often leak in.
        "DOCKER_AUTH_CONFIG",
        "NPM_TOKEN",
        "PYPI_TOKEN",
        "CARGO_REGISTRY_TOKEN",
        "NETLIFY_AUTH_TOKEN",
        "VERCEL_TOKEN",
        "CLOUDFLARE_API_TOKEN",
        "SLACK_TOKEN",
        "DISCORD_TOKEN",
    }
)

# Substring patterns — catch any var with these tokens anywhere in the name.
_SUBSTR_BLOCKED = (
    re.compile(r"SECRET", re.IGNORECASE),
    re.compile(r"PRIVATE.?KEY", re.IGNORECASE),
)


def _is_blocked(name: str) -> bool:
    upper = name.upper()
    if upper in _EXACT_BLOCKED:
        return True
    if any(upper.endswith(suf) for suf in _SUFFIX_BLOCKED):
        return True
    if any(upper.startswith(pre) for pre in _PREFIX_BLOCKED):
        return True
    if any(p.search(upper) for p in _SUBSTR_BLOCKED):
        return True
    return False


def scrub_env(
    env: Optional[Mapping[str, str]] = None,
    *,
    extra_drop: Optional[tuple] = None,
    keep: Optional[tuple] = None,
) -> MutableMapping[str, str]:
    """Return a copy of ``env`` (defaults to ``os.environ``) with every
    well-known credential variable removed.

    ``extra_drop`` is appended to the denylist. ``keep`` overrides the
    denylist (use sparingly — only when a child genuinely needs a
    credential, e.g. an explicit ``cos pkg apt-get install`` spawn).
    """
    source = os.environ if env is None else env
    keep_set = {k.upper() for k in (keep or ())}
    drop_extra = {k.upper() for k in (extra_drop or ())}
    out: MutableMapping[str, str] = {}
    for k, v in source.items():
        upper = k.upper()
        if upper in keep_set:
            out[k] = v
            continue
        if upper in drop_extra:
            continue
        if _is_blocked(k):
            continue
        out[k] = v
    return out
