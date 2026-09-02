"""claw_os_sdk — public AI/agent SDK for Claw OS, Python edition.

This package is the AI-facing surface a third-party Linux developer
uses when their app touches the kernel's AI features. Everything in
here mirrors a member of the wire protocol at ``claw-os-sdk/wire/v1/``
and is generated or hand-written to satisfy that contract.

Public modules
--------------
- :mod:`claw_os_sdk.ai`       — stable text chat wrappers around
  ``cos ai chat``. Multimodal compatibility helpers are experimental
  and currently raise :class:`claw_os_sdk.ai.AiUnsupported`.
- :mod:`claw_os_sdk.tools`    — call other apps' verbs from inside an
  app (``tools.call``, ``tools.catalog``, ``tools.for_chat``).
- :mod:`claw_os_sdk.gui`      — desktop GUI bootstrap: the kernel context
  handed to an app launched via ``cos app <id> --gui`` (toolkit-agnostic).
- :mod:`claw_os_sdk.mcp`      — manifest-bound MCP App service runtime.
- :mod:`claw_os_sdk.claw_os_session` — reference implementation for
  third-party agents that want to attach to a live ``claw-os`` session.
- :mod:`claw_os_sdk.generated` — typed dataclasses emitted from the
  wire-v1 JSON Schemas. Do not hand-edit; re-run
  ``python3 claw-os-sdk/wire/codegen.py`` instead.

Typical usage::

    from claw_os_sdk import ai, tools

    def cmd_summarise(args):
        text = open(args["path"]).read()
        return ai.chat(f"Summarise: {text}")

Capability gating (``policy.require``) and the COW snapshot helper
live in the **internal** ``cos_runtime`` package — they are
implementation details of the claw-os bundled apps, not part of the
public SDK surface.
"""

from . import ai, gui, mcp, tools

__all__ = ["ai", "gui", "mcp", "tools"]
__version__ = "0.1.0"
