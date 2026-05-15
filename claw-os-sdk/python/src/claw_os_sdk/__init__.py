"""claw_os_sdk — official multi-language SDK for Claw OS, Python edition.

This package is the canonical, versioned surface that apps and adapter
hosts use to talk to the Claw OS kernel. Every module here mirrors a
member of the wire protocol at ``claw-os-sdk/wire/v1/`` and is
generated or hand-written to satisfy that contract.

Public modules
--------------
- :mod:`claw_os_sdk.policy`   — capability gating (``policy.require``,
  ``policy.check``).
- :mod:`claw_os_sdk.ai`       — typed wrappers around ``cos ai`` (chat,
  embed, image / audio / video).
- :mod:`claw_os_sdk.tools`    — call other apps' verbs from inside an
  app (``tools.call``, ``tools.catalog``, ``tools.for_chat``).
- :mod:`claw_os_sdk.serve`    — minimal stdio MCP server SDK for apps
  whose verb surface is exposed to the agent.
- :mod:`claw_os_sdk.snapshot` — copy-on-write snapshotting helper used
  by fs / docs / kv / etc. before every gated mutation.
- :mod:`claw_os_sdk.generated` — typed dataclasses emitted from the
  wire-v1 JSON Schemas. Do not hand-edit; re-run
  ``python3 claw-os-sdk/wire/codegen.py`` instead.

Typical usage::

    from claw_os_sdk import policy, ai, tools

    def cmd_summarise(args):
        policy.require("fs.read", path=args["path"])
        text = open(args["path"]).read()
        return ai.chat([{"role": "user", "content": f"Summarise: {text}"}])

This package was previously known as ``_lib`` (under ``apps/_lib``)
and is bundled at ``/usr/lib/cos/python/claw_os_sdk`` in production
installs.
"""

from . import ai, policy, serve, snapshot, tools

__all__ = ["ai", "policy", "serve", "snapshot", "tools"]
__version__ = "0.1.0"
