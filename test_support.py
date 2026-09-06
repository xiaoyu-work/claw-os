import importlib.util
import sys
from pathlib import Path


def authenticated_mcp_params(params, *, call_id="test-call"):
    """Attach the Gateway-owned context required by an App MCP call."""

    value = dict(params or {})
    meta = dict(value.get("_meta", {}))
    meta["claw-os.dev/call-context"] = {
        "wire_version": 1,
        "call_id": call_id,
        "trace_id": "test-trace",
        "session_id": "test-session",
        "task_id": "test-task",
        "caller": {
            "kind": "system-agent",
            "id": "test-agent-session",
            "owner_uid": 1000,
        },
    }
    value["_meta"] = meta
    return value


def load_local_module(path, name, *, clear_modules=()):
    path = Path(path)
    apps_root = Path(__file__).resolve().parent / "apps"
    if str(apps_root) not in sys.path:
        sys.path.insert(0, str(apps_root))
    for prefix in clear_modules:
        for module_name in list(sys.modules):
            if module_name == prefix or module_name.startswith(f"{prefix}."):
                sys.modules.pop(module_name, None)
    sys.modules.pop(name, None)
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot load module from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except Exception:
        sys.modules.pop(name, None)
        raise
    return module
