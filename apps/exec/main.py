"""exec — Sandboxed code and command execution."""

import os
import shutil
import subprocess

DEFAULT_TIMEOUT = 300

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
        return {
            "command": command,
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }
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
        return {
            "lang": lang,
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }
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


def run(command, args):
    """Entry point called by aos."""
    if command == "run":
        return cmd_run(args)
    elif command == "script":
        return cmd_script(args)
    elif command == "which":
        return cmd_which(args)
    else:
        return {"error": f"unknown command: {command}"}
