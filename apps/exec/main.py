"""exec — Sandboxed code and command execution."""

import fcntl
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import uuid
from datetime import datetime, timezone

# Pull in the shared helpers (env scrub + atomic JSON write).
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.atomic import atomic_write_json  # noqa: E402
from _shared.env_scrub import scrub_env  # noqa: E402

from cos_runtime import policy  # noqa: E402

DEFAULT_TIMEOUT = int(os.environ.get("COS_EXEC_TIMEOUT", "300"))
MAX_OUTPUT_BYTES = 1_000_000  # 1 MB output limit for stdout/stderr
# Hard upper bound on captured output, used by the streaming Popen
# path below. Larger than the 1 MB MAX_OUTPUT_BYTES so the truncation
# marker can be inserted without further loss.
HARD_OUTPUT_CAP = 8 * 1024 * 1024  # 8 MiB per stream
WHICH_TIMEOUT = 10
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


def _first_shell_binary(tokens):
    """Best-effort: return the first real binary token from a shell-style
    argv stream, skipping leading ``VAR=value`` assignments and shell
    builtins that introduce another command (``env``, ``exec``).

    Used so ``--shell foo=bar mybin --x`` checks ``proc.spawn
    name=mybin`` rather than ``name=/bin/bash``.
    """
    SKIP_LEADING = {"env", "exec", "command", "nohup", "time", "sudo", "doas"}
    for tok in tokens:
        if not tok:
            continue
        # leading VAR=value env-overrides
        if "=" in tok and not tok.startswith("-") and "/" not in tok.split("=", 1)[0]:
            head = tok.split("=", 1)[0]
            if head and all(c.isalnum() or c == "_" for c in head) and not head[0].isdigit():
                continue
        # the bash builtin words that just introduce another command
        if tok in SKIP_LEADING:
            continue
        return tok
    return ""


def _drain_bounded(stream, cap):
    """Read all bytes from ``stream`` but keep at most ``cap`` of them.

    Used to drain the child's stdout/stderr without holding the full
    output in memory when a misbehaving child emits gigabytes. The
    overflow is consumed and dropped so the child's write end never
    blocks (deadlock).
    """
    chunks = []
    size = 0
    truncated = False
    while True:
        block = stream.read(65536)
        if not block:
            break
        if not truncated:
            room = cap - size
            if room > 0:
                if len(block) <= room:
                    chunks.append(block)
                    size += len(block)
                else:
                    chunks.append(block[:room])
                    size = cap
                    truncated = True
            else:
                truncated = True
        # else: silently discard the overflow.
    return b"".join(chunks), truncated


def _run_bounded(command, *, timeout, env):
    """Run ``command`` with bounded stdout/stderr capture and stdin=/dev/null.

    Returns ``(stdout_bytes, stderr_bytes, exit_code, truncated, timed_out)``.
    On timeout, sends SIGTERM, waits a grace period, then SIGKILL — and
    still returns whatever output was captured up to the kill.
    """
    proc = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        close_fds=True,
    )
    import threading

    out_holder = {}
    err_holder = {}

    def _drain_out():
        out_holder["data"], out_holder["truncated"] = _drain_bounded(
            proc.stdout, HARD_OUTPUT_CAP
        )

    def _drain_err():
        err_holder["data"], err_holder["truncated"] = _drain_bounded(
            proc.stderr, HARD_OUTPUT_CAP
        )

    t_out = threading.Thread(target=_drain_out, daemon=True)
    t_err = threading.Thread(target=_drain_err, daemon=True)
    t_out.start()
    t_err.start()

    timed_out = False
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            proc.terminate()
        except OSError:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                proc.kill()
            except OSError:
                pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass

    t_out.join(timeout=5)
    t_err.join(timeout=5)
    out = out_holder.get("data", b"")
    err = err_holder.get("data", b"")
    truncated = bool(out_holder.get("truncated") or err_holder.get("truncated"))
    return out, err, proc.returncode, truncated, timed_out


def _apply_output_limits(stdout_bytes, stderr_bytes, hard_truncated):
    """Decode + apply the per-stream MAX_OUTPUT_BYTES cap on top of the
    HARD_OUTPUT_CAP that the streaming drain already enforced. Appends a
    ``[truncated]`` marker so the agent knows output was clipped.
    """
    stdout = stdout_bytes.decode("utf-8", errors="replace")
    stderr = stderr_bytes.decode("utf-8", errors="replace")
    truncated = hard_truncated
    marker = "\n[truncated]"
    if len(stdout) > MAX_OUTPUT_BYTES:
        stdout = stdout[:MAX_OUTPUT_BYTES] + marker
        truncated = True
    elif truncated:
        stdout = stdout + marker
    if len(stderr) > MAX_OUTPUT_BYTES:
        stderr = stderr[:MAX_OUTPUT_BYTES] + marker
        truncated = True
    elif hard_truncated:
        stderr = stderr + marker
    return stdout, stderr, truncated


def cmd_run(args):
    """Run a command. Supports --shell and --timeout flags.

    SECURITY notes
    --------------
    * ``--shell``: when set, the resolved scope is ``proc.spawn
      name=<first real token of the script>`` (not ``/bin/bash``)
      so a wild grant to bash is not implied. If no real binary
      can be parsed out (e.g. pure pipeline / control flow), we
      require a ``proc.spawn`` wild capability.
    * Child environment is scrubbed (``scrub_env``) so the spawned
      process never sees ``OPENAI_API_KEY``, ``GITHUB_TOKEN``, AWS
      creds, etc. — credentials belong to the agent, not to whatever
      command the agent was asked to run.
    * Output is captured with a streaming bounded drain (no
      ``capture_output=True`` because that buffers the full stream
      in memory); per-stream cap is HARD_OUTPUT_CAP (8 MiB).
    """
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
        joined = " ".join(args)
        # Best-effort lex so we can pin the cap to the real program
        # name, not /bin/bash. shlex may fail on a script with odd
        # quoting, in which case we fall back to whitespace tokens.
        try:
            lex_tokens = shlex.split(joined, comments=False, posix=True)
        except ValueError:
            lex_tokens = joined.split()
        program = _first_shell_binary(lex_tokens)
        if program:
            policy.require("proc.spawn", name=program)
        else:
            # Pure shell expression with no obvious binary token —
            # require an explicit wild grant rather than silently
            # falling back to bash's own name.
            policy.require("proc.spawn", wild=True)
        command = ["/bin/bash", "-c", joined]
    else:
        command = args
        program = command[0]
        policy.require("proc.spawn", name=program)

    env = scrub_env()

    try:
        out_bytes, err_bytes, rc, hard_trunc, timed_out = _run_bounded(
            command, timeout=timeout, env=env
        )
    except FileNotFoundError:
        return {"error": f"command not found: {command[0]}"}
    except Exception as e:
        return {"error": str(e)}

    stdout, stderr, truncated = _apply_output_limits(out_bytes, err_bytes, hard_trunc)
    if timed_out:
        return {
            "command": command,
            "error": f"command timed out after {timeout}s",
            "exit_code": rc,
            "stdout": stdout,
            "stderr": stderr,
            "timed_out": True,
            "truncated": truncated or None,
        }
    resp = {
        "command": command,
        "exit_code": rc,
        "stdout": stdout,
        "stderr": stderr,
    }
    if truncated:
        resp["truncated"] = True
    return resp


def cmd_script(args):
    """Run a script inline or from a file.

    SECURITY: when a ``--file`` argument is supplied we scope the
    capability to that specific file path (``proc.spawn
    name=<basename>``). Inline-code mode still requires a wild
    ``proc.spawn`` because the code itself has no identity.
    """
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
        # Narrow the cap to the actual script file basename — a grant
        # for ``proc.spawn name=deploy.py`` should not authorise
        # running an arbitrary inline payload.
        policy.require("proc.spawn", name=os.path.basename(file_path))
    elif remaining:
        code = " ".join(remaining)
        if lang is None:
            lang = "bash"
        interpreter = LANG_INTERPRETERS.get(lang)
        if interpreter is None:
            return {"error": f"unsupported language: {lang}"}
        command = [interpreter, "-c", code]
        # Inline code: no stable identity, requires a wild grant.
        policy.require("proc.spawn", wild=True)
    else:
        return {"error": "no script or file specified"}

    env = scrub_env()

    try:
        out_bytes, err_bytes, rc, hard_trunc, timed_out = _run_bounded(
            command, timeout=timeout, env=env
        )
    except FileNotFoundError:
        return {"error": f"interpreter not found: {command[0]}"}
    except Exception as e:
        return {"error": str(e)}

    stdout, stderr, truncated = _apply_output_limits(out_bytes, err_bytes, hard_trunc)
    if timed_out:
        return {
            "lang": lang,
            "error": f"script timed out after {timeout}s",
            "exit_code": rc,
            "stdout": stdout,
            "stderr": stderr,
            "timed_out": True,
            "truncated": truncated or None,
        }
    resp = {
        "lang": lang,
        "exit_code": rc,
        "stdout": stdout,
        "stderr": stderr,
    }
    if truncated:
        resp["truncated"] = True
    return resp


def cmd_which(args):
    """Check if a command exists on the system."""
    if not args:
        return {"error": "no command name specified"}
    name = args[0]
    # Narrow the cap to the requested binary name — the old wild
    # grant let a single fs.meta wild cap turn into an
    # enumerate-every-path-on-disk gadget.
    policy.require("fs.meta", name=name)
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
    """Save the process registry to disk atomically."""
    os.makedirs(PROC_DIR, exist_ok=True)
    atomic_write_json(REGISTRY_FILE, entries)


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

    policy.require("proc.spawn", name=args[0])

    os.makedirs(PROC_DIR, exist_ok=True)

    # Use a uuid-scoped intermediate filename (hidden with a `.`
    # prefix) instead of `os.getpid()`. The parent PID is not a
    # safe uniqueness token: two concurrent `cmd_start` calls in
    # the same parent (e.g. the MCP server handling overlapping
    # requests) would collide on the same intermediate path,
    # corrupt each other's stdout/stderr files, and then both
    # rename to their own child PID's final name — losing one
    # child's early output to the other. uuid4 gives a
    # collision-free token regardless of concurrency.
    scratch = uuid.uuid4().hex[:12]
    stdout_tmp = os.path.join(PROC_DIR, f".stdout.{scratch}")
    stderr_tmp = os.path.join(PROC_DIR, f".stderr.{scratch}")

    try:
        # `with open(...)` closes the parent-side file handles
        # after Popen has dup'd them into the child. The child
        # retains its own fds; the parent doesn't need them open.
        # Scrub the environment so the background process can't
        # exfiltrate credentials the agent owns.
        env = scrub_env()
        with open(stdout_tmp, "w") as stdout_f, open(stderr_tmp, "w") as stderr_f:
            proc = subprocess.Popen(
                args,
                stdin=subprocess.DEVNULL,
                stdout=stdout_f,
                stderr=stderr_f,
                env=env,
                close_fds=True,
            )
    except FileNotFoundError:
        for p in (stdout_tmp, stderr_tmp):
            try:
                os.unlink(p)
            except OSError:
                pass
        return {"error": f"command not found: {args[0]}"}
    except Exception as e:
        for p in (stdout_tmp, stderr_tmp):
            try:
                os.unlink(p)
            except OSError:
                pass
        return {"error": str(e)}

    pid = proc.pid
    stdout_path = os.path.join(PROC_DIR, f"stdout.{pid}")
    stderr_path = os.path.join(PROC_DIR, f"stderr.{pid}")
    os.rename(stdout_tmp, stdout_path)
    os.rename(stderr_tmp, stderr_path)

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
    """Stop a background process by PID.

    SECURITY: scope is ``proc.signal name=pid:<n>`` rather than
    ``wild=True`` so a grant for stopping one PID doesn't authorise
    signalling arbitrary processes the kernel can see.
    """
    if not args:
        return {"error": "no PID specified"}
    try:
        pid = int(args[0])
    except ValueError:
        return {"error": f"invalid PID: {args[0]}"}

    policy.require("proc.signal", name=f"pid:{pid}")

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
    policy.require("proc.observe", wild=True)

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
    handlers = {
        "run": cmd_run,
        "script": cmd_script,
        "which": cmd_which,
        "start": cmd_start,
        "stop": cmd_stop,
        "ps": cmd_ps,
    }
    handler = handlers.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
