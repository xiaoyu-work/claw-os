from contextlib import contextmanager
import importlib.util
import json
import selectors
import subprocess
import sys
import tempfile
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


@contextmanager
def mcp_process(app_dir, *, env):
    """Run a staged Python App using only its manifest and public MCP wire."""
    app_dir = Path(app_dir).resolve()
    manifest = json.loads((app_dir / "app.json").read_text())
    assert manifest["runtime"] == "python"
    entry = manifest["mcp"].get("entry", "server.py")
    environment = {
        **env, "COS_APP_ID": manifest["id"],
        "COS_APP_MANIFEST": str(app_dir / "app.json"),
        "PYTHONDONTWRITEBYTECODE": "1", "PYTHONNOUSERSITE": "1",
    }
    with tempfile.TemporaryFile(mode="w+t") as errors:
        process = subprocess.Popen(
            [sys.executable, str(app_dir / entry)], cwd=app_dir, env=environment,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors, text=True,
        )
        counter = 0

        def request(method, params):
            nonlocal counter
            counter += 1
            process.stdin.write(json.dumps({
                "jsonrpc": "2.0", "id": counter, "method": method, "params": params,
            }) + "\n")
            process.stdin.flush()
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                if not selector.select(20):
                    raise TimeoutError(f"MCP fixture timed out: {method}")
                line = process.stdout.readline()
            if not line:
                errors.seek(0)
                raise AssertionError(f"MCP fixture exited: {errors.read()}")
            response = json.loads(line)
            assert response["jsonrpc"] == "2.0" and response["id"] == counter, response
            assert "result" in response, response
            return response["result"]

        try:
            request("initialize", {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "os-package-fixture", "version": "1"},
            })
            process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            process.stdin.flush()
            yield request
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
                raise
            finally:
                process.stdout.close()
        errors.seek(0)
        assert process.returncode == 0, errors.read()


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
