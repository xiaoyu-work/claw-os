"""exec — Sandboxed code and command execution."""

import fcntl
import json
import os
import shutil
import signal
import subprocess
from datetime import datetime, timezone

DEFAULT_TIMEOUT = int(os.environ.get("COS_EXEC_TIMEOUT", "300"))
MAX_OUTPUT_BYTES = 1_000_000  # 1 MB output limit for stdout/stderr
DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
PROC_DIR = os.path.join(DATA_DIR, "proc")
REGISTRY_FILE = os.path.join(PROC_DIR, "registry.json")

LANG_INTERPRETERS = {
    "python": "python3",
    "bash": "bash",
    "node": "node",
}

EXT_TO_LANG = {
    ".py": "python",
    ".sh": "bash",
    ".bash": "bash",
    ".js": "node",
}


def _parse_timeout(args):
    """Extract --timeout N from args, return (timeout, remaining_args)."""
    timeout = DEFAULT_TIMEOUT
    remaining = []
    it = iter(args)
    for arg in it:
        if arg == "--timeout":
            try:
                timeout = int(next(it))
            except (StopIteration, ValueError):
                return None, args, "invalid or missing --timeout value"
        else:
            remaining.append(arg)
    return timeout, remaining, None


def cmd_run(args):
    """Run a command. Supports --shell and --timeout flags."""
    timeout, args, err = _parse_timeout(args)
    if err:
        return {"error": err}

    shell = False
    if "--shell" in args:
        shell = True
        args = [a for a in args if a != "--shell"]

    if not args:
        return {"error": "no command specified"}

    if shell:
        command = ["/bin/bash", "-c", " ".join(args)]
    else:
        command = args

    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        stdout = result.stdout
        stderr = result.stderr
        truncated = False
        if len(stdout) > MAX_OUTPUT_BYTES:
            stdout = stdout[:MAX_OUTPUT_BYTES]
            truncated = True
        if len(stderr) > MAX_OUTPUT_BYTES:
            stderr = stderr[:MAX_OUTPUT_BYTES]
            truncated = True
        resp = {
            "command": command,
            "exit_code": result.returncode,
            "stdout": stdout,
            "stderr": stderr,
        }
        if truncated:
            resp["truncated"] = True
        return resp
    except subprocess.TimeoutExpired:
        return {"error": f"command timed out after {timeout}s"}
    except FileNotFoundError:
        return {"error": f"command not found: {command[0]}"}
    except Exception as e:
        return {"error": str(e)}


def cmd_script(args):
    """Run a script inline or from a file."""
    timeout, args, err = _parse_timeout(args)
    if err:
        return {"error": err}

    lang = None
    file_path = None
    remaining = []

    it = iter(args)
    for arg in it:
        if arg == "--lang":
            try:
                lang = next(it)
            except StopIteration:
                return {"error": "missing --lang value"}
        elif arg == "--file":
            try:
                file_path = next(it)
            except StopIteration:
                return {"error": "missing --file value"}
        else:
            remaining.append(arg)

    if file_path:
        if not os.path.isfile(file_path):
            return {"error": f"file not found: {file_path}"}
        if lang is None:
            _, ext = os.path.splitext(file_path)
            lang = EXT_TO_LANG.get(ext)
        if lang is None:
            lang = "bash"
        interpreter = LANG_INTERPRETERS.get(lang)
        if interpreter is None:
            return {"error": f"unsupported language: {lang}"}
        command = [interpreter, file_path]
    elif remaining:
        code = " ".join(remaining)
        if lang is None:
            lang = "bash"
        interpreter = LANG_INTERPRETERS.get(lang)
        if interpreter is None:
            return {"error": f"unsupported language: {lang}"}
        command = [interpreter, "-c", code]
    else:
        return {"error": "no script or file specified"}

    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        stdout = result.stdout
        stderr = result.stderr
        truncated = False
        if len(stdout) > MAX_OUTPUT_BYTES:
            stdout = stdout[:MAX_OUTPUT_BYTES]
            truncated = True
        if len(stderr) > MAX_OUTPUT_BYTES:
            stderr = stderr[:MAX_OUTPUT_BYTES]
            truncated = True
        resp = {
            "lang": lang,
            "exit_code": result.returncode,
            "stdout": stdout,
            "stderr": stderr,
        }
        if truncated:
            resp["truncated"] = True
        return resp
    except subprocess.TimeoutExpired:
        return {"error": f"script timed out after {timeout}s"}
    except FileNotFoundError:
        return {"error": f"interpreter not found: {command[0]}"}
    except Exception as e:
        return {"error": str(e)}


def cmd_which(args):
    """Check if a command exists on the system."""
    if not args:
        return {"error": "no command name specified"}
    name = args[0]
    path = shutil.which(name)
    if path:
        return {"command": name, "path": path}
    return {"command": name, "error": "not found"}


def _load_registry():
    """Load the process registry from disk."""
    if not os.path.isfile(REGISTRY_FILE):
        return []
    with open(REGISTRY_FILE, "r") as f:
        try:
            return json.load(f)
        except (json.JSONDecodeError, ValueError):
            return []


def _save_registry(entries):
    """Save the process registry to disk."""
    os.makedirs(PROC_DIR, exist_ok=True)
    with open(REGISTRY_FILE, "w") as f:
        json.dump(entries, f, indent=2)


def _with_registry_lock(fn):
    """Run fn while holding an exclusive lock on the registry."""
    os.makedirs(PROC_DIR, exist_ok=True)
    lock_path = REGISTRY_FILE + ".lock"
    with open(lock_path, "w") as lock_fd:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        try:
            return fn()
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)


def cmd_start(args):
    """Run a command in the background."""
    if not args:
        return {"error": "no command specified"}

    os.makedirs(PROC_DIR, exist_ok=True)

    try:
        proc = subprocess.Popen(
            args,
            stdout=open(os.path.join(PROC_DIR, f"stdout.{os.getpid()}"), "w"),
            stderr=open(os.path.join(PROC_DIR, f"stderr.{os.getpid()}"), "w"),
        )
    except FileNotFoundError:
        return {"error": f"command not found: {args[0]}"}
    except Exception as e:
        return {"error": str(e)}

    pid = proc.pid
    stdout_path = os.path.join(PROC_DIR, f"stdout.{pid}")
    stderr_path = os.path.join(PROC_DIR, f"stderr.{pid}")
    os.rename(os.path.join(PROC_DIR, f"stdout.{os.getpid()}"), stdout_path)
    os.rename(os.path.join(PROC_DIR, f"stderr.{os.getpid()}"), stderr_path)

    entry = {
        "pid": pid,
        "command": args,
        "started": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }

    def do_add():
        registry = _load_registry()
        registry.append(entry)
        _save_registry(registry)

    _with_registry_lock(do_add)

    return {"pid": pid, "command": args}


def cmd_stop(args):
    """Stop a background process by PID."""
    if not args:
        return {"error": "no PID specified"}
    try:
        pid = int(args[0])
    except ValueError:
        return {"error": f"invalid PID: {args[0]}"}

    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        def do_cleanup():
            registry = _load_registry()
            registry = [e for e in registry if e.get("pid") != pid]
            _save_registry(registry)

        _with_registry_lock(do_cleanup)
        return {"error": f"process {pid} not found"}
    except PermissionError:
        return {"error": f"permission denied for PID {pid}"}

    def do_remove():
        registry = _load_registry()
        registry = [e for e in registry if e.get("pid") != pid]
        _save_registry(registry)

    _with_registry_lock(do_remove)

    return {"pid": pid, "status": "stopped"}


def cmd_ps(args):
    """List running background processes."""
    def do_ps():
        registry = _load_registry()
        alive = []
        for entry in registry:
            pid = entry.get("pid")
            try:
                os.kill(pid, 0)
                alive.append(entry)
            except (ProcessLookupError, PermissionError, TypeError):
                pass
        _save_registry(alive)
        return alive

    processes = _with_registry_lock(do_ps)
    return {"processes": processes}


def _schema():
    return {
        "run": {
            "description": "Run a command with optional timeout and shell mode",
            "parameters": [
                {"name": "command", "type": "string", "required": True, "description": "Command and arguments to execute", "kind": "positional"},
                {"name": "--timeout", "type": "integer", "required": False, "description": "Timeout in seconds", "kind": "flag", "default": 300},
                {"name": "--shell", "type": "boolean", "required": False, "description": "Run via /bin/bash -c (enables shell features)", "kind": "flag", "default": False},
            ],
            "example": "cos app exec run ls -la --timeout 30",
        },
        "script": {
            "description": "Run an inline script or script file in a specified language",
            "parameters": [
                {"name": "code", "type": "string", "required": False, "description": "Inline script code (if --file not used)", "kind": "positional"},
                {"name": "--lang", "type": "string", "required": False, "description": "Language: python, bash, or node", "kind": "flag", "default": "bash"},
                {"name": "--file", "type": "string", "required": False, "description": "Path to a script file to execute", "kind": "flag"},
                {"name": "--timeout", "type": "integer", "required": False, "description": "Timeout in seconds", "kind": "flag", "default": 300},
            ],
            "example": "cos app exec script --lang python 'print(1+1)'",
        },
        "which": {
            "description": "Check if a command exists on the system",
            "parameters": [
                {"name": "name", "type": "string", "required": True, "description": "Command name to look up", "kind": "positional"},
            ],
            "example": "cos app exec which python3",
        },
        "start": {
            "description": "Run a command in the background",
            "parameters": [
                {"name": "command", "type": "string", "required": True, "description": "Command and arguments to run in background", "kind": "positional"},
            ],
            "example": "cos app exec start python3 server.py",
        },
        "stop": {
            "description": "Stop a background process by PID",
            "parameters": [
                {"name": "pid", "type": "integer", "required": True, "description": "Process ID to stop", "kind": "positional"},
            ],
            "example": "cos app exec stop 12345",
        },
        "ps": {
            "description": "List running background processes",
            "parameters": [],
            "example": "cos app exec ps",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    if command == "run":
        return cmd_run(args)
    elif command == "script":
        return cmd_script(args)
    elif command == "which":
        return cmd_which(args)
    elif command == "start":
        return cmd_start(args)
    elif command == "stop":
        return cmd_stop(args)
    elif command == "ps":
        return cmd_ps(args)
    else:
        return {"error": f"unknown command: {command}"}
