"""container-manager — Docker, Podman, and containerd inspection/control."""

import json
import os
import re
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402


RUNTIMES = frozenset({"docker", "podman", "podman-root", "containerd"})
SIGNALS = frozenset({"TERM", "KILL", "HUP", "INT", "USR1", "USR2"})
IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$")
TIMEOUT_SECS = int(os.environ.get("CLAW_CONTAINER_MANAGER_TIMEOUT", "300"))


def _cos_binary() -> str | None:
    return os.environ.get("COS_BIN") or shutil.which("cos")


def _runtime(raw: object) -> str:
    if not isinstance(raw, str) or raw not in RUNTIMES:
        raise ValueError(
            "runtime must be docker, podman, podman-root, or containerd"
        )
    return raw


def _identifier(raw: object, name: str) -> str:
    if not isinstance(raw, str) or IDENTIFIER_RE.fullmatch(raw) is None:
        raise ValueError(f"{name} is invalid")
    return raw


def _runtime_namespace(
    runtime: object,
    namespace: object | None,
) -> tuple[str, str | None]:
    checked_runtime = _runtime(runtime)
    checked_namespace = (
        _identifier(namespace, "namespace") if namespace is not None else None
    )
    if checked_runtime == "containerd":
        if checked_namespace is None:
            raise ValueError("containerd requires a namespace")
    elif checked_namespace is not None:
        raise ValueError("only containerd accepts a namespace")
    return checked_runtime, checked_namespace


def _container_args(
    runtime: object,
    target: object,
    namespace: object | None,
) -> tuple[str, str, str | None]:
    checked_runtime, checked_namespace = _runtime_namespace(runtime, namespace)
    checked_target = _identifier(target, "target")
    return checked_runtime, checked_target, checked_namespace


def _line_count(raw: object) -> int:
    if type(raw) is not int or not 1 <= raw <= 1000:
        raise ValueError("logs lines must be an integer from 1 to 1000")
    return raw


def _signal(raw: object) -> str:
    if not isinstance(raw, str) or raw not in SIGNALS:
        raise ValueError(
            "signal must be TERM, KILL, HUP, INT, USR1, or USR2"
        )
    return raw


def _require_confirmation(confirm: object) -> None:
    if confirm is not True:
        raise ValueError("remove requires confirm=true")


def _parse_payload(payload_text: str) -> dict:
    try:
        payload = json.loads(payload_text)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            "Container Manager broker returned invalid JSON"
        ) from exc
    if not isinstance(payload, dict):
        raise RuntimeError(
            "Container Manager broker returned a non-object result"
        )
    if "error" in payload:
        error = payload["error"]
        if not isinstance(error, str) or not error:
            raise RuntimeError(
                "Container Manager broker returned an invalid error payload"
            )
        raise RuntimeError(error)
    return payload


def _broker(
    action: str,
    *,
    runtime: str | None = None,
    target: str | None = None,
    namespace: str | None = None,
    lines: int | None = None,
    signal: str | None = None,
    confirm: bool = False,
) -> dict:
    cos_bin = _cos_binary()
    if not cos_bin:
        raise FileNotFoundError(
            "cos binary not found; Container Manager broker unavailable"
        )
    argv = [cos_bin, "__container", action]
    for flag, value in (
        ("--runtime", runtime),
        ("--target", target),
        ("--namespace", namespace),
        ("--lines", lines),
        ("--signal", signal),
    ):
        if value is not None:
            argv.extend([flag, str(value)])
    if confirm:
        argv.append("--confirm")
    try:
        result = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECS,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"Container Manager broker executable not found: {cos_bin}"
        ) from exc
    except PermissionError as exc:
        raise PermissionError(
            f"permission denied launching Container Manager broker: {cos_bin}"
        ) from exc
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(
            f"Container Manager broker exceeded {TIMEOUT_SECS}s for {action}"
        ) from exc

    payloads = []
    for output in (
        (result.stdout or "").strip(),
        (result.stderr or "").strip(),
    ):
        if not output:
            continue
        payloads.append(_parse_payload(output))
        if result.returncode == 0:
            break
    if result.returncode != 0:
        raise RuntimeError(f"Container Manager broker exited {result.returncode}")
    if not payloads:
        raise RuntimeError("Container Manager broker returned invalid JSON")
    return payloads[0]


def status() -> dict:
    policy.require("sys.container", name="observe")
    return _broker("status")


def list_containers(runtime: str, namespace: str | None = None) -> dict:
    runtime, namespace = _runtime_namespace(runtime, namespace)
    policy.require("sys.container", name="observe")
    return _broker("list", runtime=runtime, namespace=namespace)


def _observe_container(
    action: str,
    runtime: str,
    target: str,
    namespace: str | None,
) -> dict:
    runtime, target, namespace = _container_args(runtime, target, namespace)
    policy.require("sys.container", name="observe")
    return _broker(
        action,
        runtime=runtime,
        target=target,
        namespace=namespace,
    )


def inspect(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _observe_container("inspect", runtime, target, namespace)


def logs(
    runtime: str,
    target: str,
    lines: int = 100,
    namespace: str | None = None,
) -> dict:
    runtime, target, namespace = _container_args(runtime, target, namespace)
    lines = _line_count(lines)
    policy.require("sys.container", name="observe")
    return _broker(
        "logs",
        runtime=runtime,
        target=target,
        namespace=namespace,
        lines=lines,
    )


def processes(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _observe_container("processes", runtime, target, namespace)


def stats(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _observe_container("stats", runtime, target, namespace)


def namespaces(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _observe_container("namespaces", runtime, target, namespace)


def _control_container(
    action: str,
    runtime: str,
    target: str,
    namespace: str | None,
) -> dict:
    runtime, target, namespace = _container_args(runtime, target, namespace)
    policy.require("sys.container", name="control")
    return _broker(
        action,
        runtime=runtime,
        target=target,
        namespace=namespace,
    )


def start(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _control_container("start", runtime, target, namespace)


def stop(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _control_container("stop", runtime, target, namespace)


def restart(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _control_container("restart", runtime, target, namespace)


def pause(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _control_container("pause", runtime, target, namespace)


def unpause(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return _control_container("unpause", runtime, target, namespace)


def kill(
    runtime: str,
    target: str,
    signal: str,
    namespace: str | None = None,
) -> dict:
    runtime, target, namespace = _container_args(runtime, target, namespace)
    signal = _signal(signal)
    policy.require("sys.container", name="control")
    return _broker(
        "kill",
        runtime=runtime,
        target=target,
        namespace=namespace,
        signal=signal,
    )


def remove(
    runtime: str,
    target: str,
    namespace: str | None = None,
    *,
    confirm: bool,
) -> dict:
    runtime, target, namespace = _container_args(runtime, target, namespace)
    _require_confirmation(confirm)
    policy.require("sys.container", name="control")
    return _broker(
        "remove",
        runtime=runtime,
        target=target,
        namespace=namespace,
        confirm=True,
    )
