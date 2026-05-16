"""cos-runtime — internal helpers for claw-os bundled Python apps.

This is **not** a developer SDK. Third-party apps that just want to
call the system LLM should import :mod:`claw_os_sdk.ai` instead.

The two modules here exist so that the apps bundled inside the
claw-os repo (under ``apps/*``) can:

* :mod:`cos_runtime.policy`   — self-gate every fs / exec / pkg / net
  / secret-handling op by shelling out to ``cos perms check``.
* :mod:`cos_runtime.snapshot` — snapshot the previous state of a path
  (copy-on-write) into the current session's ``mutations.jsonl``
  before every gated mutation, so the kernel can revert.

Both shell out to the ``cos`` binary and assume the process was
spawned by the kernel with a valid ``COS_SESSION`` env var. They
will fail loudly outside that context, which is intentional.
"""

from . import policy, snapshot

__all__ = ["policy", "snapshot"]
__version__ = "0.1.0"
