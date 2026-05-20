"""cos-runtime — internal helpers for claw-os bundled Python apps.

This is **not** a developer SDK. Third-party apps that just want to
call the system LLM should import :mod:`claw_os_sdk.ai` instead.

The modules here exist so that the apps bundled inside the
claw-os repo (under ``apps/*``) can:

* :mod:`cos_runtime.policy`   — self-gate every fs / exec / pkg / net
  / secret-handling op through the hidden kernel policy bridge.
* :mod:`cos_runtime.snapshot` — snapshot the previous state of a path
  (copy-on-write) into the current session's ``mutations.jsonl``
  before every gated mutation, so the kernel can revert.
* :mod:`cos_runtime.memory`   — voluntarily push searchable summaries
  into the agent's memory so the agent can recall app activity later
  (gated by the ``memory.write`` capability bound to the app's id).

All three shell out to the ``cos`` binary and assume the process was
spawned by the kernel with a valid ``COS_SESSION`` env var. They
will fail loudly outside that context, which is intentional.
"""

from . import memory, policy, snapshot

__all__ = ["memory", "policy", "snapshot"]
__version__ = "0.1.0"
