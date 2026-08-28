"""exec — Sandboxed code and command execution."""

import fcntl
import json
import os
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
MAX_START_STDIN_BYTES = 128 * 1024
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
    * ``--shell``: arbitrary shell syntax can execute substitutions,
      functions, pipelines and commands not represented by the first token,
      so it always requires a ``proc.spawn`` wild capability.
    * Child environment is scrubbed (``scrub_env``) so the spawned
      process never sees ``OPENAI_API_KEY``, ``GITHUB_TOKEN``, AWS
      creds, etc. — credentials belong to the agent, not to whatever
      command the agent was asked to run.
    * Output is captured with a streaming bounded drain (no
      ``capture_output=True`` because that buffers the full stream
      in memory); per-stream cap is HARD_OUTPUT_CAP (8 MiB).
    """
    from canonical_argv import parse_canonical_argv
    try:
        args, options = parse_canonical_argv(
            args, value_flags={"timeout"}, bool_flags={"shell"}
        )
        timeout = int(options.get("timeout", DEFAULT_TIMEOUT))
    except (ValueError, TypeError) as error:
        return {"error": str(error)}
    shell = options.get("shell", False)

    if not args:
        return {"error": "no command specified"}

    if shell:
        joined = " ".join(args)
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
    from canonical_argv import parse_canonical_argv
    try:
        remaining, options = parse_canonical_argv(
            args, value_flags={"timeout", "lang", "file"}
        )
        timeout = int(options.get("timeout", DEFAULT_TIMEOUT))
    except (ValueError, TypeError) as error:
        return {"error": str(error)}
    lang = options.get("lang")
    file_path = options.get("file")

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
    from canonical_argv import parse_canonical_argv
    try:
        args, _ = parse_canonical_argv(args)
    except ValueError as error:
        return {"error": str(error)}
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


def _read_start_stdin():
    """Read only the invocation stdin explicitly forwarded by ``cos``."""
    data = sys.stdin.buffer.read(MAX_START_STDIN_BYTES + 1)
    if len(data) > MAX_START_STDIN_BYTES:
        raise ValueError(
            f"start stdin exceeds configured {MAX_START_STDIN_BYTES}-byte limit"
        )
    return data


def _anonymous_stdin(data):
    """Return a sealed anonymous fd positioned for one child read."""
    if not data:
        return None
    if not hasattr(os, "memfd_create"):
        raise OSError("anonymous start stdin is unavailable on this platform")
    flags = getattr(os, "MFD_CLOEXEC", 0) | getattr(os, "MFD_ALLOW_SEALING", 0)
    fd = os.memfd_create("cos-exec-stdin", flags)
    try:
        view = memoryview(data)
        while view:
            written = os.write(fd, view)
            if written <= 0:
                raise OSError("short write to anonymous start stdin")
            view = view[written:]
        os.lseek(fd, 0, os.SEEK_SET)
        if hasattr(fcntl, "F_ADD_SEALS"):
            seals = (
                fcntl.F_SEAL_SEAL
                | fcntl.F_SEAL_SHRINK
                | fcntl.F_SEAL_GROW
                | fcntl.F_SEAL_WRITE
            )
            fcntl.fcntl(fd, fcntl.F_ADD_SEALS, seals)
        return fd
    except Exception:
        os.close(fd)
        raise


def cmd_start(args):
    """Run a command in the background."""
    from canonical_argv import parse_canonical_argv
    try:
        args, _ = parse_canonical_argv(args)
    except ValueError as error:
        return {"error": str(error)}
    if not args:
        return {"error": "no command specified"}

    try:
        stdin_data = _read_start_stdin()
    except (OSError, ValueError) as error:
        return {"error": f"failed to read start stdin: {error}"}

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
    stdin_fd = None

    try:
        # `with open(...)` closes the parent-side file handles
        # after Popen has dup'd them into the child. The child
        # retains its own fds; the parent doesn't need them open.
        # Scrub the environment so the background process can't
        # exfiltrate credentials the agent owns.
        env = scrub_env()
        child_stdin = subprocess.DEVNULL
        if stdin_data:
            stdin_fd = _anonymous_stdin(stdin_data)
            child_stdin = stdin_fd
        with open(stdout_tmp, "w") as stdout_f, open(stderr_tmp, "w") as stderr_f:
            proc = subprocess.Popen(
                args,
                stdin=child_stdin,
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
    finally:
        if stdin_fd is not None:
            os.close(stdin_fd)

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
    from canonical_argv import parse_canonical_argv
    try:
        args, _ = parse_canonical_argv(args)
    except ValueError as error:
        return {"error": str(error)}
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


def run(command, args):
    """Entry point called by cos."""
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
